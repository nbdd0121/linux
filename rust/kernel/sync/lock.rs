// SPDX-License-Identifier: GPL-2.0

//! Generic kernel lock and guard.
//!
//! It contains a generic Rust lock and guard that allow for different backends (e.g., mutexes,
//! spinlocks, raw spinlocks) to be provided with minimal effort.

use super::LockClassKey;
use crate::{bindings, init::PinInit, pin_init, str::CStr, types::ScopeGuard};
use core::{cell::UnsafeCell, marker::PhantomData, marker::PhantomPinned};
use macros::pin_data;

pub mod mutex;
pub mod spinlock;

/// The "backend" of a lock.
///
/// It is the actual implementation of the lock, without the need to repeat patterns used in all
/// locks.
///
/// # Safety
///
/// - Implementers must ensure that only one thread/CPU may access the protected data once the lock
/// is owned, that is, between calls to `lock` and `unlock`.
/// - Implementers must also ensure that `relock` uses the same locking method as the original
/// lock operation. For example, it should disable interrupts if [`IrqSaveBackend::lock_irqsave`]
/// is used.
pub unsafe trait Backend<State = Self>: Sized {
    /// The state required to be kept between lock and unlock.
    type GuardState;

    /// Initialises the lock.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for write for the duration of the call, while `name` and `key` must
    /// remain valid for read indefinitely.
    unsafe fn init(
        ptr: *mut Self,
        name: *const core::ffi::c_char,
        key: *mut bindings::lock_class_key,
    ) where Self: Backend;

    /// Acquires the lock, making the caller its owner.
    #[must_use]
    fn lock(ptr: &State) -> Self::GuardState;

    /// Releases the lock, giving up its ownership.
    ///
    /// # Safety
    ///
    /// It must only be called by the current owner of the lock.
    unsafe fn unlock(ptr: &State, guard_state: &Self::GuardState);

    /// Reacquires the lock, making the caller its owner.
    ///
    /// # Safety
    ///
    /// Callers must ensure that `guard_state` comes from a previous call to [`Backend::lock`] (or
    /// variant) that has been unlocked with [`Backend::unlock`] and will be relocked now.
    unsafe fn relock(ptr: &State, guard_state: &mut Self::GuardState) {
        // SAFETY: The safety requirements ensure that the lock is initialised.
        *guard_state = Self::lock(ptr);
    }
}

/// The "backend" of a lock that supports the irq-save variant.
///
/// # Safety
///
/// The same requirements wrt mutual exclusion in [`Backend`] apply for acquiring the lock via
/// [`IrqSaveBackend::lock_irqsave`].
///
/// Additionally, when [`IrqSaveBackend::lock_irqsave`] is used to acquire the lock, implementers
/// must disable interrupts on lock, and restore interrupt state on unlock. Implementers may use
/// [`Backend::GuardState`] to store state needed to keep track of the interrupt state.
pub unsafe trait IrqSaveBackend: Backend {
    type IrqSaveBackend: Backend<Self>;
}

/// A mutual exclusion primitive.
///
/// Exposes one of the kernel locking primitives. Which one is exposed depends on the lock banckend
/// specified as the generic parameter `B`.
#[pin_data]
pub struct Lock<T: ?Sized, B: Backend> {
    /// The kernel lock object.
    #[pin]
    state: B,

    /// Some locks are known to be self-referential (e.g., mutexes), while others are architecture
    /// or config defined (e.g., spinlocks). So we conservatively require them to be pinned in case
    /// some architecture uses self-references now or in the future.
    #[pin]
    _pin: PhantomPinned,

    /// The data protected by the lock.
    pub(crate) data: UnsafeCell<T>,
}

// SAFETY: `Lock` can be transferred across thread boundaries iff the data it protects can.
unsafe impl<T: ?Sized + Send, B: Backend> Send for Lock<T, B> {}

// SAFETY: `Lock` serialises the interior mutability it provides, so it is `Sync` as long as the
// data it protects is `Send`.
unsafe impl<T: ?Sized + Send, B: Backend> Sync for Lock<T, B> {}

impl<T, B: Backend> Lock<T, B> {
    /// Constructs a new lock initialiser.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(t: T, name: &'static CStr, key: &'static LockClassKey) -> impl PinInit<Self> {
        pin_init!(Self {
            data: UnsafeCell::new(t),
            _pin: PhantomPinned,
            // SAFETY: `slot` is valid while the closure is called and both `name` and `key` have
            // static lifetimes so they live indefinitely.
            state <- unsafe { crate::init::init_from_closure::<_, core::convert::Infallible>(|slot| {
                B::init(slot, name.as_char_ptr(), key.as_ptr());
                Ok(())
            })},
        })
    }
}

impl<T: ?Sized, B: Backend> Lock<T, B> {
    /// Acquires the lock and gives the caller access to the data protected by it.
    pub fn lock(&self) -> Guard<'_, T, B> {
        // SAFETY: The constructor of the type calls `init`, so the existence of the object proves
        // that `init` was called.
        let state = B::lock(&self.state);
        // SAFETY: The lock was just acquired.
        unsafe { Guard::new(self, state) }
    }
}

impl<T: ?Sized, B: IrqSaveBackend> Lock<T, B> {
    /// Acquires the lock and gives the caller access to the data protected by it.
    ///
    /// Before acquiring the lock, it disables interrupts. When the guard is dropped, the interrupt
    /// state (either enabled or disabled) is restored to its state before
    /// [`lock_irqsave`](Self::lock_irqsave) was called.
    pub fn lock_irqsave(&self) -> Guard<'_, T, B, B::IrqSaveBackend> {
        // SAFETY: The constructor of the type calls `init`, so the existence of the object proves
        // that `init` was called.
        let state = B::IrqSaveBackend::lock(&self.state);
        // SAFETY: The lock was just acquired.
        unsafe { Guard::new(self, state) }
    }
}

/// A lock guard.
///
/// Allows mutual exclusion primitives that implement the `Backend` trait to automatically unlock
/// when a guard goes out of scope. It also provides a safe and convenient way to access the data
/// protected by the lock.
#[must_use = "the lock unlocks immediately when the guard is unused"]
pub struct Guard<'a, T: ?Sized, B: Backend, E: Backend<B> = B> {
    pub(crate) lock: &'a Lock<T, B>,
    pub(crate) state: E::GuardState,
    _not_send: PhantomData<*mut ()>,
}

// SAFETY: `Guard` is sync when the data protected by the lock is also sync.
unsafe impl<T: Sync + ?Sized, B: Backend, E: Backend<B>> Sync for Guard<'_, T, B, E> {}

impl<T: ?Sized, B: Backend, E: Backend<B>> Guard<'_, T, B, E> {
    pub(crate) fn do_unlocked(&mut self, cb: impl FnOnce()) {
        // SAFETY: The caller owns the lock, so it is safe to unlock it.
        unsafe { E::unlock(&self.lock.state, &self.state) };

        // SAFETY: The lock was just unlocked above and is being relocked now.
        let _relock =
            ScopeGuard::new(|| unsafe { E::relock(&self.lock.state, &mut self.state) });

        cb();
    }
}

impl<T: ?Sized, B: Backend, E: Backend<B>> core::ops::Deref for Guard<'_, T, B, E> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: The caller owns the lock, so it is safe to deref the protected data.
        unsafe { &*self.lock.data.get() }
    }
}

impl<T: ?Sized, B: Backend, E: Backend<B>> core::ops::DerefMut for Guard<'_, T, B, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: The caller owns the lock, so it is safe to deref the protected data.
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T: ?Sized, B: Backend, E: Backend<B>> Drop for Guard<'_, T, B, E> {
    fn drop(&mut self) {
        // SAFETY: The caller owns the lock, so it is safe to unlock it.
        unsafe { E::unlock(&self.lock.state, &self.state) };
    }
}

impl<'a, T: ?Sized, B: Backend, E: Backend<B>> Guard<'a, T, B, E> {
    /// Constructs a new immutable lock guard.
    ///
    /// # Safety
    ///
    /// The caller must ensure that it owns the lock.
    pub(crate) unsafe fn new(lock: &'a Lock<T, B>, state: E::GuardState) -> Self {
        Self {
            lock,
            state,
            _not_send: PhantomData,
        }
    }
}
