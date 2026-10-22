// SPDX-License-Identifier: GPL-2.0
/*
 * Copyright (C) 2026 Red Hat Inc.
 * Author: Philipp Stanner <pstanner@redhat.com>
 */

//! DRM's job dependency manager and load balancer for GPUs with firmware
//! scheduling, i.e., GPUs that can spawn one ring exclusive per user context.
//!
//! JobQueue abstracts the entire handling of [`dma_buf::FenceContext`] and its
//! associated [`dma_buf::DriverFence`]s for the user.
//!
//! In this design, a JobQueue is always the owner of a [`dma_buf::FenceContext`]
//! which represents the timeline on the associated GPU ring. Jobs created on
//! this JobQueue always contain a [`dma_buf::DriverFence`] from this JobQueue,
//! where the fence sequence number corresponds with the job sequence number,
//! and the fence's generic payload data is also the job's payload data. This
//! allows for the dma_fence backend_ops callbacks interacting with the driver
//! through JobQueue. The trait [`JobQueueOps`] is the interface between driver
//! and fence backend_ops.
//!
//! [`Job`]s created on the [`JobQueue`] take over ownership of the user's data
//! and prepare all memory allocations. A later job submission sets up the fence
//! and performs no fallible operations.
//!
//! Before jobs are submitted to a queue, dependencies in the form of
//! [`dma_buf::Fence`] can be set on them. Jobs are run by the JobQueue in
//! process context through a work item in FIFO order. Only jobs whose
//! dependency fences have all been signaled are run. If the front job's
//! dependency is not yet fullfilled, the JobQueue pauses until that's the case.
//!
//! JobQueue implements a credit count system, where the queue itself gets
//! assigned a capacity by the user, and jobs get assigned individual cost. Jobs
//! get only pushed if there is enough capacity; if not, running stops until
//! that is the case. These credits must accurately represent the capacity on
//! the GPU ring the queue was created for.
//!
//! The driver tells the JobQueue which jobs are finished through
//! [`JobQueue::complete_jobs_up_to_seqno()`]. This function is typically
//! invoked through the driver's GPU interrupt handler.
//!
//! Since jobs might contain data that must not drop in atomic context, and
//! since jobs' [`dma_buf::DriverFence`]s perform a call to
//! [`rcu::synchronize_rcu()`], they are dropped through a work item if [`Drop`]
//! is implemented for the job's payload data.
//!
//! From JobQueue's perspective, it is not necessary to call any of its API
//! functions with a driver lock held; this is actually discouraged to reduce
//! the probability of deadlocks. All JobQueue API functions are properly
//! ordered using the internal lock.
//!
//! JobQueue tells drivers to run jobs through [`JobQueueOps::run_job`], which
//! is called without the JobQueue's spinlock being held, i.e., the driver is
//! permitted to perform sleeping operations in the callback. JobQueue handles
//! races between `run_job` and [`JobQueueOps::complete_jobs_up_to_seqno()`].
//!
//! This is pure Rust code that does not directly abstract C.

#![allow(clippy::multiple_bound_locations)] // Clippy false positive, to be addressed by pin-init.

use core::{
    convert::Infallible,
    ops::Deref, //
};

use kernel::{
    dma_buf::dma_fence::{
        DriverFence,
        DriverFenceAllocation,
        Fence,
        FenceCallback,
        FenceCallbackRegistration,
        FenceContext,
        FenceContextOps, //
    },
    list::{
        impl_list_arc_safe,
        impl_list_item,
        List,
        ListItem,
        ListLinks, //
    },
    new_srcu,
    prelude::*,
    str::CString,
    sync::{
        aref::ARef,
        atomic::{
            Atomic,
            Relaxed, //
        }, //
        new_spinlock_irq,
        SpinLockIrq,
    }, //
    sync::{
        atomic::AtomicFlag,
        Srcu, //
    },
    sync::{Arc, UniqueArc},
    workqueue::{
        self,
        new_scoped_work,
        //        ScopedQueue,
        ScopedWork,
        ScopedWorkItem,
        ScopedWorkRef,
    }, //
};

/// A linked list where all elements are owned.
///
/// This is a thin wrapper of `List` with all elements are `UniqueArc`.
struct UniqueList<T: ListItem>(List<T>);

impl<T: ListItem> Default for UniqueList<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ListItem> UniqueList<T> {
    fn new() -> Self {
        Self(List::new())
    }

    fn push_front(&mut self, item: Pin<UniqueArc<T>>) {
        self.0.push_front(item.into())
    }

    fn push_back(&mut self, item: Pin<UniqueArc<T>>) {
        self.0.push_back(item.into())
    }

    fn pop_front(&mut self) -> Option<Pin<UniqueArc<T>>> {
        Some(
            Arc::into_unique_or_drop(self.0.pop_front()?.into_arc())
                .expect("UniqueList should only contain UniqueArc"),
        )
    }
}

struct DependencyWaker<'b, 'nr_of_deps, T: JobQueueOps + Send + Sync> {
    nr_of_deps: &'nr_of_deps Atomic<u32>,
    jq: &'b JobQueue<T>,
}

impl<'b, T: JobQueueOps + Send + Sync> FenceCallback for DependencyWaker<'b, '_, T> {
    fn on_signal(&mut self) {
        // Last dependency kicks the worker.
        if self.nr_of_deps.fetch_sub(1, Relaxed) == 1 {
            // The drop-order of JobQueue ensures that all DependencyWakers are
            // cancelled before the JobQueue itself is dropped.
            self.jq.check_start_submit_worker();
        }
    }
}

/// A JobQueue job before it has been submitted.
///
/// This is only useful for registering dependencies on and for submitting it.
pub struct Job<'a, T: JobQueueOps + Send + Sync> {
    inner: Pin<UniqueArc<JobInternal<'a, T>>>,
}

impl<'a, T: JobQueueOps + Send + Sync> Job<'a, T> {
    /// Add a dependency for the job.
    ///
    /// A JobQueue will only push a job once `fence` has been signaled.
    pub fn add_dependency(&mut self, fence: &Fence) -> Result {
        self.inner.as_mut().with_project(|job| {
            // If the registration below gets registered successfully, a signaling
            // fence will decrement the atomic. That could race. Thus, increment in
            // advance. Ordering is ensured thanks to the dma_fence backend.
            job.nr_of_deps.fetch_add(1, Relaxed);

            let dependency = try_pin_init!(Dependency {
                links <- ListLinks::new(),
                // SAFETY: The `registration` is not forgotten. It drops with JobQueue.
                registration <- unsafe {
                    FenceCallbackRegistration::new(
                        fence,
                        DependencyWaker {
                            nr_of_deps: job.nr_of_deps,
                            jq: job.jq,
                        },
                    )
                }
            });
            match UniqueArc::pin_init(dependency, GFP_KERNEL) {
                Err(e) => {
                    let _ = job.nr_of_deps.fetch_sub(1, Relaxed);
                    if e == ENOENT {
                        // Was already signaled. No reason to bother the caller.
                        return Ok(());
                    }
                    return Err(e);
                }
                Ok(dep) => {
                    job.dependencies.push_back(dep);
                }
            }

            Ok(())
        })
    }

    /// Submit the job.
    ///
    /// Returns a [`dma_buf::Fence`] on which the user can register callbacks.
    /// The fence will be signaled once the job is completed through
    /// [`JobQueue::complete_jobs_up_to_seqno`], which can happen immediately
    /// after the job has been submitted.
    ///
    /// This function does all fence initialization under the JobQueue lock in
    /// one go, including setting of the fence sequence number. Thus, it is
    /// typically unnecessary (and advaised against) to call this function with
    /// a driver lock held.
    pub fn submit(self) -> ARef<Fence> {
        let jq = self.inner.jq;
        jq.with_inner(|inner| {
            // SAFETY: this is a lifetime transmute from `Job<'queue, T>` to `Job<'fctx, T>`.
            //
            // `job` internally holds a reference to `JobQueue`, while we are now moving `job` into
            // `inner`, which may borrow from `fctx` but not the full `JobQueue`. So on surface this
            // would be incorrect. However, as we drain `inner` inside `JobQueue::drop`, the
            // reference is gone before any field is destructed.
            let job = unsafe { core::mem::transmute::<Job<'_, T>, Job<'_, T>>(self) };

            // The lock orders setting of the sequence number, so no driver lock is needed.
            let mut inner = inner.lock();
            let mut job = job.inner;
            let fence = job
                .as_mut()
                .project()
                .fence_allocation
                .take()
                .expect("fence_allocation not present")
                .new_fence();

            let ret_fence = ARef::from(fence.as_fence());

            *job.as_mut().project().fence = Some(fence);

            inner.waiting_jobs.push_back(job);
            drop(inner);
            jq.check_start_submit_worker();

            ret_fence
        })
    }
}

#[pin_data]
struct Dependency<'b, 'nr_of_deps, T: JobQueueOps + Send + Sync> {
    #[pin]
    links: ListLinks,
    #[pin]
    registration: FenceCallbackRegistration<DependencyWaker<'b, 'nr_of_deps, T>>,
}

impl_list_arc_safe! {
    impl{'b, T: JobQueueOps + Send + Sync} ListArcSafe<0> for Dependency<'b, '_, T> { untracked; }
}
impl_list_item! {
    impl{'b, T: JobQueueOps + Send + Sync} ListItem<0> for Dependency<'b, '_, T> { using ListLinks { self.links }; }
}

#[pin_data]
#[repr(C)]
struct JobInternal<'b, T: JobQueueOps + Send + Sync> {
    #[pin]
    links: ListLinks,
    cost: u32,
    fence_allocation: Option<DriverFenceAllocation<'b, DriverDataWrapper<T>>>,
    fence: Option<DriverFence<'b, DriverDataWrapper<T>>>,
    dependencies: UniqueList<Dependency<'b, 'nr_of_deps, T>>,
    nr_of_deps: Atomic<u32>,
    jq: &'b JobQueue<T>,
}

impl_list_arc_safe! {
    impl{'b, T: JobQueueOps + Send + Sync} ListArcSafe<0> for JobInternal<'b, T> { untracked; }
}
impl_list_item! {
    impl{'b, T: JobQueueOps + Send + Sync} ListItem<0> for JobInternal<'b, T> { using ListLinks { self.links }; }
}

struct SubmitWorker<'a, 'fctx, T: JobQueueOps + Send + Sync> {
    srcu: &'a Srcu,
    revoked: &'a AtomicFlag,
    inner: &'a SpinLockIrq<JobQueueInner<'fctx, T>>,
    fctx: &'fctx FenceContext<DriverDataWrapper<T>>,
}

impl<T: JobQueueOps + Send + Sync> ScopedWorkItem for SubmitWorker<'_, '_, T> {
    fn run(work: &ScopedWorkRef<Self>) {
        // Synchronize against revocation in `JobQueue::drop`.
        let _lock_guard = work.srcu.read_lock();
        if work.revoked.load(Relaxed) {
            return;
        }

        let mut jq = work.inner.lock();

        while let Some(job) = jq.as_mut().project().waiting_jobs.pop_front() {
            // While we didn't hold the lock below for `run_job()`, it could be
            // that a new job entered the waiting list which has an unfulfilled
            // dependency.
            if job.nr_of_deps.load(Relaxed) > 0 {
                jq.waiting_jobs.push_front(job);
                break;
            }
            let new_capacity = jq.capacity as i64 - i64::from(job.cost);
            if new_capacity < 0 {
                jq.waiting_jobs.push_front(job);
                break;
            }
            *jq.as_mut().project().capacity = new_capacity as u64;

            let seqno = job
                .fence
                .as_ref()
                .expect("fence not present")
                .as_fence()
                .seqno();
            let runnable_job = JobRunnable {
                seqno,
                data: job.fence.as_ref().expect("fence not present").deref(),
            };

            let jq_data = work.fctx.deref().deref();

            drop(jq); // Release lock.
            T::run_job(jq_data, runnable_job);
            jq = work.inner.lock(); // Retake lock.

            // run_job() above needs to be called without lock protection. Thus,
            // it is possible that the driver instructed completion right before
            // we could re-take the lock.
            if jq.last_completed >= seqno {
                if let Some(fence) = &job.fence {
                    fence.signal(Ok(()));
                } else {
                    panic!("fence not present.");
                }

                // Drop here directly if possible. Otherwise, let the DropWorker
                // take care of it.
                if core::mem::needs_drop::<T::JobDataType>() {
                    jq.drop_list.push_back(job);
                }
            } else {
                jq.running_jobs.push_back(job);
            }
        }
    }
}

struct DropWorker<'a, 'fctx, T: JobQueueOps + Send + Sync> {
    srcu: &'a Srcu,
    revoked: &'a AtomicFlag,
    inner: &'a SpinLockIrq<JobQueueInner<'fctx, T>>,
}

impl<T: JobQueueOps + Send + Sync> ScopedWorkItem for DropWorker<'_, '_, T> {
    fn run(work: &ScopedWorkRef<Self>) {
        // Synchronize against revocation in `JobQueue::drop`.
        let _lock_guard = work.srcu.read_lock();
        if work.revoked.load(Relaxed) {
            return;
        }

        let mut jq = work.inner.lock();

        while let Some(job) = jq.drop_list.pop_front() {
            drop(jq); // Release lock.
            drop(job);
            jq = work.inner.lock(); // Retake lock.
        }
    }
}

#[pin_data]
struct JobQueueInner<'a, T: JobQueueOps + Send + Sync> {
    /// Fence sequence number of the last job the user has completed.
    #[pin]
    /// List of jobs waiting to be run on the hardware.
    waiting_jobs: UniqueList<JobInternal<'a, T>>,
    #[pin]
    /// Jobs that are currently executing on the hardware.
    running_jobs: UniqueList<JobInternal<'a, T>>,
    /// The queue's capacity.
    capacity: u64,
    /// The sequence number of the last completed job.
    last_completed: u64,
    #[pin]
    /// Jobs that have finished executing and whose payload data `T` implements
    /// `Drop`. These jobs are dropped in process context, which also allows for
    /// their [`dma_buf::DriverFence`]s to implement the necessary RCU grace
    /// period delay.
    ///
    /// If `Drop` is not implemented, this list is unused and the elements drop
    /// directly in [`JobQueue::complete_jobs_up_to_seqno`].
    ///
    /// Should the JobQueue drop and the [`DropWorker`] not yet have dropped all
    /// jobs, that is no problem because the JobQueue itself drops in process
    /// context.
    drop_list: UniqueList<JobInternal<'a, T>>,
}

/// A JobQueue.
///
/// When you drop your JobQueue, it might still attempt for one RCU grace period
/// to submit more jobs via the [`JobQueueOps::run_job`] callback into your driver.
///
/// Dropping a JobQueue results in all remaining waiting and running jobs'
/// fences being signaled with ECANCELED.
///
/// A JobQueue must only ever drop in non-atomic / process context.
///
/// # Examples
///
/// ```
/// use core::ops::Deref;
///
/// use kernel::{
///     str::CString,
///     dma_buf::*,
///     drm::{
///         JobQueue,
///         JobQueueOps,
///         JobRunnable,
///     },
/// };
///
/// let driver_name = CString::try_from_fmt(fmt!("dummy_driver"))?;
/// let timeline_name = CString::try_from_fmt(fmt!("dummy_timeline"))?;
///
/// #[pin_data]
/// struct FenceContextData {}
///
/// impl FenceContextData {
///     fn new() -> impl PinInit<Self> {
///         pin_init!(Self {})
///     }
/// }
///
/// impl FenceContextOps for FenceContextData {
///     type FenceDataType = ();
/// }
///
/// let fctx_data = FenceContextData::new();
///
/// let fctx = KBox::pin_init(
///     FenceContext::new(0, c"dependency_driver", c"dependency_timeline", fctx_data),
///     GFP_KERNEL
/// )?;
///
/// let dependency = fctx.new_fence_allocation(())?.new_fence();
///
/// #[pin_data]
/// struct JqData {}
///
/// impl JqData {
///     fn new() -> impl PinInit<Self> {
///         pin_init!(Self {})
///     }
/// }
///
/// struct JobData {
///     data: CString,
/// }
///
/// impl JobQueueOps for JqData {
///     type JobDataType = JobData;
///
///     fn run_job<'a>(&self, job: JobRunnable<'a, JobData>) {
///
///     }
/// }
///
/// let jq_data = JqData::new();
/// let jq = KBox::pin_init(JobQueue::new(0, driver_name, timeline_name, 1000, jq_data), GFP_KERNEL)?;
///
/// struct Callback {}
///
/// impl FenceCallback for Callback {
///     fn on_signal(&mut self) {
///
///     }
/// }
///
/// let data = CString::try_from_fmt(fmt!("hello there!"))?;
/// let data = JobData { data };
/// Ok::<(), Error>(())
/// ```
#[pin_data(PinnedDrop)]
pub struct JobQueue<T: JobQueueOps + Send + Sync> {
    #[pin]
    #[uses('_: invariant, 'revoked: invariant, 'srcu: invariant)]
    drop_worker: ScopedWork<DropWorker<'inner, 'fctx, T>>,
    #[pin]
    #[uses('_: invariant)]
    submit_worker: ScopedWork<SubmitWorker<'inner, 'fctx, T>>,
    #[pin]
    #[uses('fctx: invariant)]
    inner: SpinLockIrq<JobQueueInner<'fctx, T>>,
    #[pin]
    fctx: FenceContext<DriverDataWrapper<T>>,
    revoked: AtomicFlag,
    #[pin]
    srcu: Srcu,
}

// SAFETY: The JobQueue is locked and pinned.
unsafe impl<T: JobQueueOps + Send + Sync> Sync for JobQueue<T> {}
// SAFETY: The JobQueue is locked and pinned.
unsafe impl<T: JobQueueOps + Send + Sync> Send for JobQueue<T> {}

/// Object passed to the driver to run a job.
pub struct JobRunnable<'a, T> {
    /// The job's sequence number.
    pub seqno: u64,
    /// The associated job data.
    pub data: &'a T,
}

/// Ops for the JobQueue.
pub trait JobQueueOps {
    /// The type of the data passed over in [`JobQueue::new_job`].
    type JobDataType: Send + Sync;

    /// Instructs the driver to run this job.
    ///
    /// JobQueue calls this callback from process context without any locks
    /// held, thus, the driver is permitted to sleep in its implementation.
    ///
    /// The driver must be aware that JobQueue trusts on its credit count system
    /// for this operation to be infallible.
    ///
    /// This callback might be called while the driver is in the process of
    /// dropping the JobQueue, but will never be called afterwards.
    fn run_job<'a>(&self, job: JobRunnable<'a, Self::JobDataType>);
}

#[pin_data]
struct DriverDataWrapper<T: JobQueueOps + Send + Sync> {
    #[pin]
    data: T,
}

impl<T: JobQueueOps + Send + Sync> Deref for DriverDataWrapper<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T: JobQueueOps + Send + Sync> DriverDataWrapper<T> {
    fn new<E>(data: impl PinInit<T, E>) -> impl PinInit<Self, E>
    where
        Error: From<E>,
        E: From<Infallible>,
    {
        try_pin_init!(Self {
            data <- data,
        }? E)
    }
}

impl<T: JobQueueOps + Send + Sync> FenceContextOps for DriverDataWrapper<T> {
    type FenceDataType = T::JobDataType;
}

impl<T: JobQueueOps + Send + Sync> JobQueue<T> {
    /// Create a new JobQueue.
    pub fn new<E>(
        initial_seqno: u64,
        // TODO: pass these as &CStr. Figure out how to get the lifetimes for PinInit right.
        driver_name: CString,
        timeline_name: CString,
        capacity: u64,
        data: impl PinInit<T, E>,
    ) -> impl PinInit<Self, Error>
    where
        Error: From<E>,
        E: From<Infallible>,
    {
        try_pin_init!(Self {
            srcu <- new_srcu!(),
            revoked: AtomicFlag::new(false),
            fctx <- FenceContext::new(initial_seqno, &driver_name, &timeline_name, DriverDataWrapper::new(data)),
            inner <- new_spinlock_irq!(try_pin_init!(JobQueueInner {
                capacity,
                last_completed: 0,
                waiting_jobs: UniqueList::new(),
                running_jobs: UniqueList::new(),
                drop_list: UniqueList::new(),
            })),
            submit_worker <- new_scoped_work!("JobQueueSubmitWorker", Ok(SubmitWorker {
                srcu: Pin::get_ref(srcu),
                revoked,
                inner: Pin::get_ref(inner),
                fctx: Pin::get_ref(fctx)
            })),
            drop_worker <- new_scoped_work!("JobQueueDropWorker", Ok(DropWorker {
                srcu: Pin::get_ref(srcu),
                revoked,
                inner: Pin::get_ref(inner)
            })),
        })
    }

    /// Create a new job.
    ///
    /// The `cost` specified here is what JobQueue subtracts from its capacity
    /// to determine whether a job is runnable, i.e., has space on the GPU ring.
    ///
    /// The `data` contained by a job will be borrowed back to the driver once
    /// the jobs shall be run through [`JobQueueOps::run_job`].
    pub fn new_job<'a>(&'a self, cost: u32, data: T::JobDataType) -> Result<Job<'a, T>> {
        let fence_allocation = self.fctx.new_fence_allocation(data)?;
        let job = try_pin_init!(JobInternal {
            links <- ListLinks::new(),
            cost,
            fence_allocation: Some(fence_allocation),
            fence: None,
            nr_of_deps: Atomic::new(0),
            dependencies: UniqueList::new(),
            jq: &self,
        });

        let inner = UniqueArc::pin_init(job, GFP_KERNEL)?;

        Ok(Job { inner })
    }

    fn check_start_submit_worker(&self) {
        self.with_submit_worker(|submit_worker| {
            // TODO this shouldn't be the system wq
            // SAFETY: The worker is carried by the JobQueue and, thus, cannot be
            // forgotten.
            let _ = unsafe { workqueue::system_dfl().enqueue_scoped(submit_worker) };
        })
    }

    fn check_start_drop_worker(&self) {
        self.with_drop_worker(|drop_worker| {
            // TODO this shouldn't be the system wq
            // SAFETY: The worker is carried by the JobQueue and, thus, cannot be
            // forgotten.
            let _ = unsafe { workqueue::system_dfl().enqueue_scoped(drop_worker) };
        })
    }

    /// Complete all the jobs in the queue's running_list, up to `seqno`.
    ///
    /// This signals the jobs' fences and schedules dropping of both the [`Job`]
    /// and its [`DriverFence`].
    ///
    /// This signals the associated fences with the `Ok(())` status.
    ///
    /// This function tells the implementation that there is now more capacity
    /// for more jobs to be pushed into the GPU ring.
    // TODO RFC: should seqno be inclusive? [0,seqno] <-> [0,seqno[ ?
    pub fn complete_jobs_up_to_seqno(&self, seqno: u64) {
        self.with_inner(|inner| {
            let needs_drop = core::mem::needs_drop::<T::JobDataType>();
            let mut jq_locked = inner.lock();
            let mut jq = jq_locked.as_mut().project();

            if seqno < *jq.last_completed {
                return;
            }

            while let Some(mut job) = jq.running_jobs.pop_front() {
                if job
                    .as_mut()
                    .project()
                    .fence
                    .as_mut()
                    .expect("fence not present")
                    .as_fence()
                    .seqno()
                    > seqno
                {
                    jq.running_jobs.push_front(job);
                    break;
                }

                *jq.capacity += u64::from(job.cost);

                if let Some(fence) = &job.fence {
                    fence.signal(Ok(()));
                } else {
                    panic!("fence not present.");
                }

                if needs_drop {
                    jq.drop_list.push_back(job);
                }
                // Otherwise, job is dropped here directly. Its fence won't perform
                // an RCU grace period wait.
            }

            // This enables the SubmitWorker to complete a job that is not yet
            // in 'running_jobs', in case of a race against the lock.
            *jq.last_completed = seqno;
            drop(jq_locked);

            self.check_start_submit_worker();
            if needs_drop {
                self.check_start_drop_worker();
            }
        })
    }

    /// Tear the JobQueue down. Complete all pending jobs and signal their
    /// fences with `error`.
    ///
    /// Only use this if you want to tear the JobQueue down in an error path and
    /// want to specify the error.
    ///
    /// You can also just drop the JobQueue, which would result in all remaining
    /// jobs' fences being signaled with ECANCELED.
    ///
    /// Note that this function has a small race window against the
    /// `SubmitWorker` and its [`JobQueueOps::run_job`] callback, where the
    /// latter might theoretically succeed at pushing more jobs to the driver.
    /// The driver likely will want to ignore such attempts to push more jobs to
    /// the GPU ring. Besides, the race merely results in these subsequent
    /// jobs being signaled with ECANCELED on drop, thus, maybe differing from
    /// the `error` provided here.
    ///
    /// This function must be called from non-atomic context.
    pub fn teardown<E>(self, error: Error)
    where
        Error: From<E>,
    {
        self.with_inner(|inner| {
            let mut jq = inner.lock();

            // We don't drop the jobs and fences here while we hold the lock. Just
            // signal with the error. The dropping lists will then drop them locklessly.

            while let Some(job) = jq.running_jobs.pop_front() {
                if let Some(fence) = &job.fence {
                    fence.signal(Err(error));
                } else {
                    panic!("fence not present.");
                }

                jq.drop_list.push_back(job);
            }
        })
    }
}

#[pinned_drop]
impl<T: JobQueueOps + Send + Sync> PinnedDrop for JobQueue<T> {
    fn drop(self: Pin<&mut Self>) {
        // Revoke to prevent still existing DependencyWakers from kicking the
        // SubmitWorker again before we could deregister them.
        self.revoked.store(true, Relaxed);
        self.srcu.synchronize();
        self.with_inner(|inner| {
            let mut inner = inner.lock();
            let waiting_jobs = core::mem::take(&mut inner.waiting_jobs);
            let running_jobs = core::mem::take(&mut inner.running_jobs);
            let drop_list = core::mem::take(&mut inner.drop_list);
            drop(inner);
            drop(waiting_jobs);
            drop(running_jobs);
            drop(drop_list);
        })
    }
}
