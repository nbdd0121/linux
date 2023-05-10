// SPDX-License-Identifier: GPL-2.0

//! Work queues.
//!
//! C header: [`include/linux/workqueue.h`](../../../../include/linux/workqueue.h)

use crate::projection::Field;
use crate::{bindings, prelude::*, sync::Arc, types::Opaque};
use core::marker::{PhantomData, PhantomPinned};

/// A kernel work queue.
///
/// Wraps the kernel's C `struct workqueue_struct`.
///
/// It allows work items to be queued to run on thread pools managed by the kernel. Several are
/// always available, for example, `system`, `system_highpri`, `system_long`, etc.
#[repr(transparent)]
pub struct Queue(Opaque<bindings::workqueue_struct>);

// SAFETY: Kernel workqueues are usable from any thread.
unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Queue {
    /// Use the provided `workqueue_struct` with Rust.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the provided raw pointer is not dangling, that it points at a
    /// valid workqueue, and that it remains valid until the end of 'a.
    pub unsafe fn from_raw<'a>(ptr: *const bindings::workqueue_struct) -> &'a Queue {
        // SAFETY: The `Queue` type is `#[repr(transparent)]`, so the pointer cast is valid. The
        // caller promises that the pointer is not dangling.
        unsafe { &*(ptr as *const Queue) }
    }

    /// Enqueues a work item.
    ///
    /// This may fail if the work item is already enqueued in a workqueue.
    pub fn enqueue<
        T: WorkItem<F, C>,
        F: Field<T, Type = Work<T, F, C>>,
        C: WorkContainer<T> + Send + 'static,
    >(
        &self,
        w: C,
    ) -> Result<(), C> {
        let queue_ptr = self.0.get();

        let ptr = C::into_raw(w);
        let work = unsafe { F::map(ptr) };
        let work_ptr = unsafe { Work::raw_get(work) };

        // SAFETY: There are two cases.
        //
        //  1. If `queue_work_on` returns false, then we failed to push the work item to the queue.
        //     In this case, we don't touch the work item again.
        //
        //  2. If `queue_work_on` returns true, then we pushed the work item to the queue. The work
        //     queue will call the function pointer in the `work_struct` at some point in the
        //     future. We require `T` to be static, so the type has no lifetimes annotated on it.
        //     We require `T` to be send, so there are no thread-safety issues to take care of.
        if unsafe { bindings::queue_work_on(bindings::WORK_CPU_UNBOUND as _, queue_ptr, work_ptr) }
        {
            Ok(())
        } else {
            Err(unsafe { C::from_raw(ptr) })
        }
    }

    /// Tries to spawn the given function or closure as a work item.
    ///
    /// Users are encouraged to use [`spawn_work_item`] as it automatically defines the lock class
    /// key to be used.
    pub fn try_spawn<T: 'static + Send + Fn()>(&self, func: T) -> Result {
        #[pin_data]
        #[derive(macros::Field)]
        struct ClosureWork<T> {
            #[pin]
            work: Work<ClosureWork<T>, crate::field!(work), Pin<Box<ClosureWork<T>>>>,
            func: Option<T>,
        }

        impl<T> ClosureWork<T> {
            fn project(self: Pin<&mut Self>) -> &mut Option<T> {
                // SAFETY: The `func` field is not structurally pinned.
                unsafe { &mut self.get_unchecked_mut().func }
            }
        }

        impl<T: FnOnce()> WorkItem<crate::field!(work), Pin<Box<ClosureWork<T>>>> for ClosureWork<T> {
            fn run(mut container: Pin<Box<ClosureWork<T>>>) {
                if let Some(func) = container.as_mut().project().take() {
                    (func)()
                }
            }
        }

        let init = pin_init!(ClosureWork {
            work <- Work::new(),
            func: Some(func),
        });

        self.enqueue::<_, crate::field!(work), _>(Box::pin_init(init)?)
            .unwrap_or_else(|_| unreachable!());
        Ok(())
    }
}

/// Links for a work item.
///
/// This struct contains a function pointer to the `T::run` function from the `WorkItemAdapter`
/// trait, and defines the linked list pointers necessary to enqueue a work item in a workqueue.
///
/// Wraps the kernel's C `struct work_struct`.
///
/// This is a helper type used to associate a `work_struct` with the `WorkItemAdapter` that uses
/// it.
#[repr(transparent)]
pub struct Work<T, F: Field<T>, C: WorkContainer<T>> {
    work: Opaque<bindings::work_struct>,
    _pin: PhantomPinned,
    _adapter: PhantomData<(T, F, C)>,
}

///
pub trait WorkContainer<T> {
    unsafe fn from_raw(ptr: *const T) -> Self;
    fn into_raw(this: Self) -> *const T;
}

impl<T> WorkContainer<T> for Pin<Box<T>> {
    unsafe fn from_raw(ptr: *const T) -> Self {
        unsafe { Pin::new_unchecked(Box::from_raw(ptr as _)) }
    }

    fn into_raw(this: Self) -> *const T {
        Box::into_raw(unsafe { Pin::into_inner_unchecked(this) }) as _
    }
}

impl<T> WorkContainer<T> for Arc<T> {
    unsafe fn from_raw(ptr: *const T) -> Self {
        unsafe { Arc::from_raw(ptr) }
    }

    fn into_raw(this: Self) -> *const T {
        Arc::into_raw(this)
    }
}

pub trait WorkItem<F, C>: Sized
where
    F: Field<Self, Type = Work<Self, F, C>>,
    C: WorkContainer<Self>,
{
    fn run(container: C);
}

// SAFETY: Kernel work items are usable from any thread.
//
// We do not need to constrain `T` since the work item does not actually contain a `T`.
unsafe impl<T, F: Field<T>, C: WorkContainer<T>> Send for Work<T, F, C> {}
unsafe impl<T, F: Field<T>, C: WorkContainer<T>> Sync for Work<T, F, C> {}

impl<T, F: Field<T>, C: WorkContainer<T>> Work<T, F, C> {
    /// Creates a new instance of [`Work`].
    #[inline]
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> impl PinInit<Self>
    where
        T: WorkItem<F, C>,
        F: Field<T, Type = Work<T, F, C>>,
    {
        unsafe extern "C" fn run<
            T: WorkItem<F, C>,
            F: Field<T, Type = Work<T, F, C>>,
            C: WorkContainer<T>,
        >(
            ptr: *mut bindings::work_struct,
        ) {
            let ptr = ptr as *mut Work<T, F, C>;
            let f = unsafe { F::reverse_map(ptr) };
            let container = unsafe { C::from_raw(f) };
            T::run(container);
        }

        // SAFETY: The `WorkItemAdapter` implementation promises that `T::run` can be used as the
        // work item function.
        unsafe {
            kernel::init::pin_init_from_closure(move |slot| {
                bindings::__INIT_WORK(Self::raw_get(slot), Some(run::<T, F, C>), false);
                Ok(())
            })
        }
    }

    /// Get a pointer to the inner `work_struct`.
    ///
    /// # Safety
    ///
    /// The provided pointer must not be dangling. (But it need not be initialized.)
    #[inline]
    pub unsafe fn raw_get(ptr: *const Self) -> *mut bindings::work_struct {
        // SAFETY: The caller promises that the pointer is valid.
        //
        // A pointer cast would also be ok due to `#[repr(transparent)]`. We use `addr_of!` so that
        // the compiler does not complain that `work` is unused.
        unsafe { Opaque::raw_get(core::ptr::addr_of!((*ptr).work)) }
    }
}

// === built-in queues ===

/// Returns the system work queue (`system_wq`).
///
/// It is the one used by schedule\[_delayed\]_work\[_on\](). Multi-CPU multi-threaded. There are
/// users which expect relatively short queue flush time.
///
/// Callers shouldn't queue work items which can run for too long.
pub fn system() -> &'static Queue {
    // SAFETY: `system_wq` is a C global, always available.
    unsafe { Queue::from_raw(bindings::system_wq) }
}

/// Returns the system high-priority work queue (`system_highpri_wq`).
///
/// It is similar to the one returned by [`system`] but for work items which require higher
/// scheduling priority.
pub fn system_highpri() -> &'static Queue {
    // SAFETY: `system_highpri_wq` is a C global, always available.
    unsafe { Queue::from_raw(bindings::system_highpri_wq) }
}

/// Returns the system work queue for potentially long-running work items (`system_long_wq`).
///
/// It is similar to the one returned by [`system`] but may host long running work items. Queue
/// flushing might take relatively long.
pub fn system_long() -> &'static Queue {
    // SAFETY: `system_long_wq` is a C global, always available.
    unsafe { Queue::from_raw(bindings::system_long_wq) }
}

/// Returns the system unbound work queue (`system_unbound_wq`).
///
/// Workers are not bound to any specific CPU, not concurrency managed, and all queued work items
/// are executed immediately as long as `max_active` limit is not reached and resources are
/// available.
pub fn system_unbound() -> &'static Queue {
    // SAFETY: `system_unbound_wq` is a C global, always available.
    unsafe { Queue::from_raw(bindings::system_unbound_wq) }
}

/// Returns the system freezable work queue (`system_freezable_wq`).
///
/// It is equivalent to the one returned by [`system`] except that it's freezable.
pub fn system_freezable() -> &'static Queue {
    // SAFETY: `system_freezable_wq` is a C global, always available.
    unsafe { Queue::from_raw(bindings::system_freezable_wq) }
}

/// Returns the system power-efficient work queue (`system_power_efficient_wq`).
///
/// It is inclined towards saving power and is converted to "unbound" variants if the
/// `workqueue.power_efficient` kernel parameter is specified; otherwise, it is similar to the one
/// returned by [`system`].
pub fn system_power_efficient() -> &'static Queue {
    // SAFETY: `system_power_efficient_wq` is a C global, always available.
    unsafe { Queue::from_raw(bindings::system_power_efficient_wq) }
}

/// Returns the system freezable power-efficient work queue (`system_freezable_power_efficient_wq`).
///
/// It is similar to the one returned by [`system_power_efficient`] except that is freezable.
pub fn system_freezable_power_efficient() -> &'static Queue {
    // SAFETY: `system_freezable_power_efficient_wq` is a C global, always available.
    unsafe { Queue::from_raw(bindings::system_freezable_power_efficient_wq) }
}
