// SPDX-License-Identifier: GPL-2.0

//! Work queues.
//!
//! C header: [`include/linux/workqueue.h`](../../../../include/linux/workqueue.h)

use crate::{bindings, types::Opaque};

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
    pub fn enqueue<T: WorkItem + Send + 'static>(&self, w: T) -> T::EnqueueOutput {
        let queue_ptr = self.0.get();

        // SAFETY: There are two cases.
        //
        //  1. If `queue_work_on` returns false, then we failed to push the work item to the queue.
        //     In this case, we don't touch the work item again.
        //
        //  2. If `queue_work_on` returns true, then we pushed the work item to the queue. The work
        //     queue will call the function pointer in the `work_struct` at some point in the
        //     future. We require `T` to be static, so the type has no lifetimes annotated on it.
        //     We require `T` to be send, so there are no thread-safety issues to take care of.
        //
        // In either case we follow the safety requirements of `__enqueue`.
        unsafe {
            w.__enqueue(move |work_ptr| {
                bindings::queue_work_on(bindings::WORK_CPU_UNBOUND as _, queue_ptr, work_ptr)
            })
        }
    }
}

/// A work item.
///
/// This is the low-level trait that is designed for being as general as possible.
///
/// # Safety
///
/// Implementers must ensure that `__enqueue` behaves as documented.
pub unsafe trait WorkItem {
    /// The return type of `Queue::enqueue`.
    type EnqueueOutput;

    /// Enqueues this work item on a queue using the provided `queue_work_on` method.
    ///
    /// # Safety
    ///
    /// Calling this method guarantees that the provided closure will be called with a raw pointer
    /// to a `work_struct`. The closure should behave in the following way:
    ///
    ///  1. If the `work_struct` cannot be pushed to a workqueue because its already in one, then
    ///     the closure should return `false`. It may not access the pointer after returning
    ///     `false`.
    ///  2. If the `work_struct` is successfully added to a workqueue, then the closure should
    ///     return `true`. When the workqueue executes the work item, it will do so by calling the
    ///     function pointer stored in the `work_struct`. The work item ensures that the raw
    ///     pointer remains valid until that happens.
    ///
    /// This method may not have any other failure cases than the closure returning `false`. The
    /// output type should reflect this, but it may also be an infallible type if the work item
    /// statically ensures that pushing the `work_struct` will succeed.
    ///
    /// If the work item type is annotated with any lifetimes, then the workqueue must call the
    /// function pointer before any such lifetime expires. (Or it may forget the work item and
    /// never call the function pointer at all.)
    ///
    /// If the work item type is not Send, then the work item must be executed on the same thread
    /// as the call to `__enqueue`.
    unsafe fn __enqueue<F>(self, queue_work_on: F) -> Self::EnqueueOutput
    where
        F: FnOnce(*mut bindings::work_struct) -> bool;
}
