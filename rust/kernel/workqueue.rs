// SPDX-License-Identifier: GPL-2.0

//! Work queues.
//!
//! C header: [`include/linux/workqueue.h`](../../../../include/linux/workqueue.h)

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

/// Defines the method that should be called when a work item is executed.
///
/// This trait is used when the `work_struct` field is defined using the `Work` helper.
///
/// # Safety
///
/// Implementers must ensure that `__enqueue` uses a `work_struct` initialized with the `run`
/// method of this trait as the function pointer.
pub unsafe trait WorkItemAdapter: WorkItem {
    /// Run this work item.
    ///
    /// # Safety
    ///
    /// Must only be called via the function pointer that `__enqueue` provides to the
    /// `queue_work_on` closure, and only as described in the documentation of `queue_work_on`.
    unsafe extern "C" fn run(ptr: *mut bindings::work_struct);
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
pub struct Work<T: ?Sized> {
    work: Opaque<bindings::work_struct>,
    _pin: PhantomPinned,
    _adapter: PhantomData<T>,
}

// SAFETY: Kernel work items are usable from any thread.
//
// We do not need to constrain `T` since the work item does not actually contain a `T`.
unsafe impl<T: ?Sized> Send for Work<T> {}
unsafe impl<T: ?Sized> Sync for Work<T> {}

impl<T: ?Sized> Work<T> {
    /// Creates a new instance of [`Work`].
    #[inline]
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> impl PinInit<Self>
    where
        T: WorkItemAdapter,
    {
        // SAFETY: The `WorkItemAdapter` implementation promises that `T::run` can be used as the
        // work item function.
        unsafe {
            kernel::init::pin_init_from_closure(move |slot| {
                bindings::__INIT_WORK(Self::raw_get(slot), Some(T::run), false);
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

/// Declares that a type has a `Work<T>` field.
///
/// # Safety
///
/// The OFFSET constant must be the offset of a field in Self of type `Work<T>`. The methods on
/// this trait must have exactly the behavior that the definitions given below have.
pub unsafe trait HasWork<T> {
    /// The offset of the `Work<T>` field.
    const OFFSET: usize;

    /// Returns the offset of the `Work<T>` field.
    ///
    /// This method exists because the OFFSET constant cannot be accessed if the type is not Sized.
    #[inline]
    fn get_work_offset(&self) -> usize {
        Self::OFFSET
    }

    /// Returns a pointer to the `Work<T>` field.
    ///
    /// # Safety
    ///
    /// The pointer must not be dangling. (But the memory need not be initialized.)
    #[inline]
    unsafe fn raw_get_work(ptr: *mut Self) -> *mut Work<T>
    where
        Self: Sized,
    {
        // SAFETY: The caller promises that the pointer is not dangling.
        unsafe { (ptr as *mut u8).add(Self::OFFSET) as *mut Work<T> }
    }

    /// Returns a pointer to the struct containing the `Work<T>` field.
    ///
    /// # Safety
    ///
    /// The pointer must not be dangling. (But the memory need not be initialized.)
    #[inline]
    unsafe fn work_container_of(ptr: *mut Work<T>) -> *mut Self
    where
        Self: Sized,
    {
        // SAFETY: The caller promises that the pointer is not dangling.
        unsafe { (ptr as *mut u8).sub(Self::OFFSET) as *mut Self }
    }
}

/// Used to safely implement the `HasWork<T>` trait.
///
/// # Examples
///
/// ```
/// use kernel::sync::Arc;
///
/// struct MyStruct {
///     work_field: Work<Arc<MyStruct>>,
/// }
///
/// impl_has_work! {
///     impl HasWork<Arc<MyStruct>> for MyStruct { self.work_field }
/// }
/// ```
#[macro_export]
macro_rules! impl_has_work {
    ($(impl$(<$($implarg:ident),*>)?
       HasWork<$work_type:ty>
       for $self:ident $(<$($selfarg:ident),*>)?
       { self.$field:ident }
    )*) => {$(
        // SAFETY: The implementation of `raw_get_work` only compiles if the field has the right
        // type.
        unsafe impl$(<$($implarg),*>)? $crate::workqueue::HasWork<$work_type> for $self $(<$($selfarg),*>)? {
            const OFFSET: usize = $crate::offset_of!(Self, $field) as usize;

            #[inline]
            unsafe fn raw_get_work(ptr: *mut Self) -> *mut $crate::workqueue::Work<$work_type> {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of_mut!((*ptr).$field)
                }
            }
        }
    )*};
}

// === ArcWorkItem ===

/// Declares that `Arc<Self>` should implement `WorkItem`.
///
/// # Examples
///
/// The example below will make `Arc<MyStruct>` implement the WorkItem trait so that you can
/// enqueue it in a workqueue.
///
/// ```
/// use kernel::sync::Arc;
///
/// struct MyStruct {
///     work_field: Work<Arc<MyStruct>>,
/// }
///
/// kernel::impl_has_work! {
///     impl HasWork<Arc<MyStruct>> for MyStruct { self.work_field }
/// }
///
/// impl ArcWorkItem for MyStruct {
///     fn run(self: Arc<Self>) {
///         pr_info!("Executing MyStruct on a workqueue.");
///     }
/// }
/// ```
pub trait ArcWorkItem {
    /// Called when this work item is executed.
    fn run(self: Arc<Self>);
}

unsafe impl<T> WorkItem for Arc<T>
where
    T: ArcWorkItem + HasWork<Self> + ?Sized,
{
    type EnqueueOutput = Result<(), Self>;

    unsafe fn __enqueue<F>(self, queue_work_on: F) -> Self::EnqueueOutput
    where
        F: FnOnce(*mut bindings::work_struct) -> bool,
    {
        let ptr = Arc::into_raw(self);

        // Using `get_work_offset` here for object-safety.
        //
        // SAFETY: The pointer is valid since we just got it from `into_raw`.
        let off = unsafe { (&*ptr).get_work_offset() };

        // SAFETY: The `HasWork` impl promises that this offset gives us a field of type
        // `Work<Self>` in the same allocation.
        let work_ptr = unsafe { (ptr as *const u8).add(off) as *const Work<Self> };
        // SAFETY: The pointer is not dangling.
        let work_ptr = unsafe { Work::raw_get(work_ptr) };

        match (queue_work_on)(work_ptr) {
            true => Ok(()),
            // SAFETY: The work queue has not taken ownership of the pointer.
            false => Err(unsafe { Arc::from_raw(ptr) }),
        }
    }
}

// Let `Work<Arc<T>>` be usable with types that are `ArcWorkItem`.
//
// We do not allow unsized types here. The `Work<Arc<T>>` field should always specify the actual
// concrete type stored in the `Arc`.
//
// SAFETY: The `Work<Arc<T>>` field must be initialized with this run method because the `Work`
// struct prevents you from initializing it in any other way. The `__enqueue` trait uses the
// same `Work<Arc<T>>` field because `HasWork` promises to always return the same field.
unsafe impl<T> WorkItemAdapter for Arc<T>
where
    T: ArcWorkItem + HasWork<Self> + Sized,
{
    unsafe extern "C" fn run(ptr: *mut bindings::work_struct) {
        // SAFETY: The `__enqueue` method always uses a `work_struct` stored in a `Work<Self>`.
        let ptr = ptr as *mut Work<Self>;
        // SAFETY: This computes the pointer that `__enqueue` got from `Arc::into_raw`.
        let ptr = unsafe { T::work_container_of(ptr) };
        // SAFETY: This pointer comes from `Arc::into_raw` and we've been given back ownership.
        let arc = unsafe { Arc::from_raw(ptr) };

        arc.run();
    }
}

// === BoxWorkItem ===

/// Declares that `Pin<Box<Self>>` should implement `WorkItem`.
///
/// # Examples
///
/// The example below will make `Pin<Box<MyStruct>>` implement the WorkItem trait so that you can
/// enqueue it in a workqueue.
///
/// ```
/// struct MyStruct {
///     work_field: Work<Pin<Box<MyStruct>>>,
/// }
///
/// kernel::impl_has_work! {
///     impl HasWork<Pin<Box<MyStruct>>> for MyStruct { self.work_field }
/// }
///
/// impl BoxWorkItem for MyStruct {
///     fn run(self: Pin<Box<MyStruct>>) {
///         pr_info!("Executing MyStruct on a workqueue.");
///     }
/// }
/// ```
pub trait BoxWorkItem {
    /// Called when this work item is executed.
    fn run(self: Pin<Box<Self>>);
}

unsafe impl<T> WorkItem for Pin<Box<T>>
where
    T: BoxWorkItem + HasWork<Self> + ?Sized,
{
    // When a box is in a workqueue, the workqueue has exclusive ownership of the box. Therefore,
    // it's not possible to enqueue a box while it is in a workqueue.
    type EnqueueOutput = ();

    unsafe fn __enqueue<F>(self, queue_work_on: F)
    where
        F: FnOnce(*mut bindings::work_struct) -> bool,
    {
        // SAFETY: We will not used the contents in an unpinned manner.
        let ptr = unsafe { Box::into_raw(Pin::into_inner_unchecked(self)) };

        // Using `get_work_offset` here for object-safety.
        //
        // SAFETY: The pointer is valid since we just got it from `into_raw`.
        let off = unsafe { (&*ptr).get_work_offset() };

        // SAFETY: The `HasWork` impl promises that this offset gives us a field of type
        // `Work<Self>` in the same allocation.
        let work_ptr = unsafe { (ptr as *mut u8).add(off) as *mut Work<Self> };
        // SAFETY: The pointer is not dangling.
        let work_ptr = unsafe { Work::raw_get(work_ptr) };

        match (queue_work_on)(work_ptr) {
            true => {}
            // SAFETY: This method requires exclusive ownership of the box, so it cannot be in a
            // workqueue.
            false => unsafe { core::hint::unreachable_unchecked() },
        }
    }
}

// Let `Work<Pin<Box<T>>>` be usable with types that are `BoxWorkItem`.
//
// We do not allow unsized types here. The `Work<Pin<Box<T>>>` field should always specify the actual
// concrete type stored in the `Box`.
//
// SAFETY: The `Work<Pin<Box<T>>>` field must be initialized with this run method because the `Work`
// struct prevents you from initializing it in any other way. The `__enqueue` trait uses the
// same `Work<Pin<Box<T>>>` field because `HasWork` promises to always return the same field.
unsafe impl<T> WorkItemAdapter for Pin<Box<T>>
where
    T: BoxWorkItem + HasWork<Self> + Sized,
{
    unsafe extern "C" fn run(ptr: *mut bindings::work_struct) {
        // SAFETY: The `__enqueue` method always uses a `work_struct` stored in a `Work<Self>`.
        let ptr = ptr as *mut Work<Self>;
        // SAFETY: This computes the pointer that `__enqueue` got from `Arc::into_raw`.
        let ptr = unsafe { T::work_container_of(ptr) };
        // SAFETY: This pointer comes from `Box::into_raw` and we've been given back ownership.
        // The box was originally pinned, so pinning it again is ok.
        let boxed = unsafe { Pin::new_unchecked(Box::from_raw(ptr)) };

        boxed.run();
    }
}

// === Newtype adapter macro ===

/// Helper macro for structs with several `Work` fields that can be in several queues at once.
///
/// For each `Work` field in your type `T`, a newtype struct that wraps an `Arc<T>` or
/// `Pin<Box<T>>` should be defined.
///
/// # Examples
///
/// ```
/// struct MyStruct {
///     work1: Work<MyStructWork1>,
///     work2: Work<MyStructWork2>,
/// }
///
/// impl_has_work! {
///     impl HasWork<MyStructWork1> for MyStruct { self.work1 }
///     impl HasWork<MyStructWork2> for MyStruct { self.work2 }
/// }
///
/// define_work_adapter_newtype! {
///     struct MyStructWork1(Arc<MyStruct>);
///     struct MyStructWork2(Arc<MyStruct>);
/// }
///
/// impl MyStructWork1 {
///     fn run(self) {
///         // ...
///     }
/// }
///
/// impl MyStructWork2 {
///     fn run(self) {
///         // ...
///     }
/// }
/// ```
/// This will let you push an `MyStructWork1(arc)` or `MyStructWork2(arc)` to a work queue. The arc
/// can be in two work queues at the same time, and the `run` method on the wrapper type is called
/// when the work item is called.
#[macro_export]
macro_rules! define_work_adapter_newtype {
    (
        $(#[$outer:meta])*
        $pub:vis struct $name:ident(
            $(#[$innermeta:meta])*
            $fpub:vis Arc<$inner:ty> $(,)?
        );
        $($rest:tt)*
    ) => {
        $(#[$outer])*
        $pub struct $name($(#[$innermeta])* $fpub $crate::sync::Arc<$inner>);

        unsafe impl $crate::workqueue::WorkItem for $name {
            type EnqueueOutput = ::core::result::Result<(), $name>;

            unsafe fn __enqueue<F>(self, queue_work_on: F) -> Self::EnqueueOutput
            where
                F: ::core::ops::FnOnce(*mut $crate::bindings::work_struct) -> bool,
            {
                let ptr = $crate::sync::Arc::into_raw(self.0);

                // SAFETY: The pointer is not dangling since we just got it from Arc::into_raw.
                let work_ptr = unsafe { <$inner as $crate::workqueue::HasWork::<$name>>::raw_get_work(ptr.cast_mut()) };

                // SAFETY: The pointer is not dangling.
                let work_ptr = unsafe { $crate::workqueue::Work::raw_get(work_ptr) };

                match (queue_work_on)(work_ptr) {
                    true => Ok(()),
                    // SAFETY: The work queue has not taken ownership of the pointer.
                    false => Err($name(unsafe { $crate::sync::Arc::from_raw(ptr) })),
                }
            }
        }

        unsafe impl $crate::workqueue::WorkItemAdapter for $name {
            unsafe extern "C" fn run(ptr: *mut $crate::bindings::work_struct) {
                // SAFETY: The `__enqueue` method always uses a `work_struct` stored in a `Work<Self>`.
                let ptr = ptr as *mut $crate::workqueue::Work<Self>;
                // SAFETY: This computes the pointer that `__enqueue` got from `Arc::into_raw`.
                let ptr = unsafe { <$inner as $crate::workqueue::HasWork::<$name>>::work_container_of(ptr) };
                // SAFETY: This pointer comes from `Arc::into_raw` and we've been given back ownership.
                let arc = unsafe { $crate::sync::Arc::from_raw(ptr) };

                $name::run($name(arc));
            }
        }

        define_work_adapter_newtype! { $($rest)* }
    };

    (
        $(#[$outer:meta])*
        $pub:vis struct $name:ident(
            $(#[$innermeta:meta])*
            $fpub:vis Pin<Box<$inner:ty>> $(,)?
        );
        $($rest:tt)*
    ) => {
        $(#[$outer])*
        $pub struct $name($(#[$innermeta])* $fpub ::core::pin::Pin<::alloc::boxed::Box<$inner>>);

        unsafe impl $crate::workqueue::WorkItem for $name {
            type EnqueueOutput = ();

            unsafe fn __enqueue<F>(self, queue_work_on: F)
            where
                F: ::core::ops::FnOnce(*mut $crate::bindings::work_struct) -> bool,
            {
                // SAFETY: We will not used the contents in an unpinned manner.
                let boxed = unsafe { ::core::pin::Pin::into_inner_unchecked(self.0) };
                let ptr = ::alloc::boxed::Box::into_raw(boxed);

                // SAFETY: The pointer is not dangling since we just got it from Box::into_raw.
                let work_ptr = unsafe { <$inner as $crate::workqueue::HasWork::<$name>>::raw_get_work(ptr) };

                // SAFETY: The pointer is not dangling.
                let work_ptr = unsafe { $crate::workqueue::Work::raw_get(work_ptr) };

                match (queue_work_on)(work_ptr) {
                    true => {},
                    // SAFETY: This method requires exclusive ownership of the box, so it cannot be in a
                    // workqueue.
                    false => unsafe { ::core::hint::unreachable_unchecked() },
                }
            }
        }

        unsafe impl $crate::workqueue::WorkItemAdapter for $name {
            unsafe extern "C" fn run(ptr: *mut $crate::bindings::work_struct) {
                // SAFETY: The `__enqueue` method always uses a `work_struct` stored in a `Work<Self>`.
                let ptr = ptr as *mut $crate::workqueue::Work<Self>;
                // SAFETY: This computes the pointer that `__enqueue` got from `Arc::into_raw`.
                let ptr = unsafe { <$inner as $crate::workqueue::HasWork::<$name>>::work_container_of(ptr) };
                // SAFETY: This pointer comes from `Box::into_raw` and we've been given back ownership.
                let boxed = unsafe { ::alloc::boxed::Box::from_raw(ptr) };
                // SAFETY: The box was originally pinned, so pinning it again is ok.
                let boxed = unsafe { ::core::pin::Pin::new_unchecked(boxed) };

                $name::run($name(boxed));
            }
        }

        define_work_adapter_newtype! { $($rest)* }
    };

    // After processing the last definition, we call ourselves with no input.
    () => {};
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
