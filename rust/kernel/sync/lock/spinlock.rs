// SPDX-License-Identifier: GPL-2.0

//! A kernel spinlock.
//!
//! This module allows Rust code to use the kernel's `spinlock_t`.

use super::IrqSaveBackend;
use crate::bindings;
use crate::types::Opaque;

/// Creates a [`SpinLock`] initialiser with the given name and a newly-created lock class.
///
/// It uses the name if one is given, otherwise it generates one based on the file name and line
/// number.
#[macro_export]
macro_rules! new_spinlock {
    ($inner:expr $(, $name:literal)? $(,)?) => {
        $crate::sync::SpinLock::new(
            $inner, $crate::optional_name!($($name)?), $crate::static_lock_class!())
    };
}

/// A spinlock.
///
/// Exposes the kernel's [`spinlock_t`]. When multiple CPUs attempt to lock the same spinlock, only
/// one at a time is allowed to progress, the others will block (spinning) until the spinlock is
/// unlocked, at which point another CPU will be allowed to make progress.
///
/// Instances of [`SpinLock`] need a lock class and to be pinned. The recommended way to create such
/// instances is with the [`pin_init`](crate::pin_init) and [`new_spinlock`] macros.
///
/// # Examples
///
/// The following example shows how to declare, allocate and initialise a struct (`Example`) that
/// contains an inner struct (`Inner`) that is protected by a spinlock.
///
/// ```
/// use kernel::{init::InPlaceInit, init::PinInit, new_spinlock, pin_init, sync::SpinLock};
///
/// struct Inner {
///     a: u32,
///     b: u32,
/// }
///
/// #[pin_data]
/// struct Example {
///     c: u32,
///     #[pin]
///     d: SpinLock<Inner>,
/// }
///
/// impl Example {
///     fn new() -> impl PinInit<Self> {
///         pin_init!(Self {
///             c: 10,
///             d <- new_spinlock!(Inner { a: 20, b: 30 }),
///         })
///     }
/// }
///
/// // Allocate a boxed `Example`.
/// let e = Box::pin_init(Example::new())?;
/// assert_eq!(e.c, 10);
/// assert_eq!(e.d.lock().a, 20);
/// assert_eq!(e.d.lock().b, 30);
/// assert_eq!(e.d.lock_irqsave().a, 20);
/// assert_eq!(e.d.lock_irqsave().b, 30);
/// ```
///
/// The following example shows how to use interior mutability to modify the contents of a struct
/// protected by a spinlock despite only having a shared reference:
///
/// ```
/// use kernel::sync::SpinLock;
///
/// struct Example {
///     a: u32,
///     b: u32,
/// }
///
/// fn example(m: &SpinLock<Example>) {
///     let mut guard = m.lock();
///     guard.a += 10;
///     guard.b += 20;
/// }
///
/// fn example2(m: &SpinLock<Example>) {
///     let mut guard = m.lock_irqsave();
///     guard.a += 10;
///     guard.b += 20;
/// }
/// ```
///
/// [`spinlock_t`]: ../../../../include/linux/spinlock.h
pub type SpinLock<T> = super::Lock<T, SpinLockBackend>;

/// A kernel `spinlock_t` lock backend.
pub struct SpinLockBackend(Opaque<bindings::spinlock_t>);

pub struct SpinLockIrqSaveBackend;

// SAFETY: The underlying kernel `spinlock_t` object ensures mutual exclusion. `relock` uses the
// same scheme as `unlock` to figure out which locking method was used originally.
unsafe impl super::Backend for SpinLockBackend {
    type GuardState = ();

    unsafe fn init(
        ptr: *mut Self,
        name: *const core::ffi::c_char,
        key: *mut bindings::lock_class_key,
    ) {
        // SAFETY: The safety requirements ensure that `ptr` is valid for writes, and `name` and
        // `key` are valid for read indefinitely.
        unsafe {
            bindings::__spin_lock_init(
                Opaque::raw_get(core::ptr::addr_of_mut!((*ptr).0)),
                name,
                key,
            )
        }
    }

    fn lock(state: &Self) -> Self::GuardState {
        // SAFETY: The safety requirements of this function ensure that `ptr` points to valid
        // memory, and that it has been initialised before.
        unsafe { bindings::spin_lock(state.0.get()) };
        ()
    }

    unsafe fn unlock(state: &Self, _: &Self::GuardState) {
        // SAFETY: The safety requirements of this function ensure that `ptr` is valid and that
        // the caller is the owner of the mutex.
        unsafe { bindings::spin_unlock(state.0.get()) }
    }
}

unsafe impl super::Backend<SpinLockBackend> for SpinLockIrqSaveBackend {
    type GuardState = core::ffi::c_ulong;

    // This function has an unsatisfised bound and thus can never be called.
    // But Rust currently still require us to provide an implementation.
    unsafe fn init(_: *mut Self, _: *const core::ffi::c_char, _: *mut bindings::lock_class_key) {
        crate::build_error!("function found is unsatisfisable");
    }

    fn lock(state: &SpinLockBackend) -> Self::GuardState {
        // SAFETY: The safety requirements of this function ensure that `ptr` points to valid
        // memory, and that it has been initialised before.
        unsafe { bindings::spin_lock_irqsave(state.0.get()) }
    }

    unsafe fn unlock(state: &SpinLockBackend, guard_state: &Self::GuardState) {
        // SAFETY: The safety requirements of this function ensure that `ptr` is valid and that
        // the caller is the owner of the mutex.
        unsafe { bindings::spin_unlock_irqrestore(state.0.get(), *guard_state) }
    }

    unsafe fn relock(state: &SpinLockBackend, _guard_state: &mut Self::GuardState) {
        _ = Self::lock(state);
    }
}

// SAFETY: The underlying kernel `spinlock_t` object ensures mutual exclusion. We use the `irqsave`
// variant of the C lock acquisition functions to disable interrupts and retrieve the original
// interrupt state, and the `irqrestore` variant of the lock release functions to restore the state
// in `unlock` -- we use the guard context to determine which method was used to acquire the lock.
unsafe impl IrqSaveBackend for SpinLockBackend {
    type IrqSaveBackend = SpinLockIrqSaveBackend;
}
