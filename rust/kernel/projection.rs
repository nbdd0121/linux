pub use macros::{Field, PinField};
use core::pin::Pin;
use core::mem::MaybeUninit;

/// Representation of a field name.
///
/// A field name `x` is represented with `FieldName<{field_name_hash("x")}>`.
pub struct FieldName<const N: u64>(());

pub const fn field_name_hash(name: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    let mut i = 0;
    while i < name.len() {
        hash ^= name.as_bytes()[i] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        i += 1;
    }
    hash
}

/// Information of a field of struct `Base`.
///
/// # Safety
/// The field must represent a field named `NAME` in a type `Base` that has the type `Type`.
/// The `map` function must be implemented such that it returns a pointer to the field.
///
/// This trait should not be implemented manually; instead, use the `#[derive(Field)]` instead.
pub unsafe trait Field<Base> {
    /// The type of the field.
    type Type: ?Sized;
    /// The name of the field.
    const NAME: &'static str;

    /// Adjust the pointer from the containing struct to the field.
    ///
    /// # Safety
    /// `ptr` must be a non-null and aligned pointer to `Self::Base`.
    unsafe fn map(ptr: *const Base) -> *const Self::Type;
}

/// Implemented for types that has field and therefore can be projected.
pub trait HasField {}

/// Trait for a wrapper type that can be projected to a field.
///
/// `F` is a descriptor of a field (`FieldName` with some generic parameters).
pub trait Projectable<T, F: Field<T>> {
    /// Type of the wrapped projected field.
    type Target;

    /// Project the field.
    ///
    /// # Safety
    /// The function must be called only if `F` is accessible with Rust privacy
    /// rules by the caller.
    unsafe fn project(self) -> Self::Target;

    #[doc(hidden)]
    unsafe fn project_with_check(this: Self, check: fn(&T)) -> Self::Target
    where
        Self: Sized,
    {
        unsafe { Self::project(this) }
    }
}

impl<'a, T, F> Projectable<T, F> for &'a MaybeUninit<T>
where
    F: Field<T>,
    F::Type: Sized + 'a,
{
    type Target = &'a MaybeUninit<F::Type>;

    unsafe fn project(self) -> Self::Target {
        // SAFETY: Projecting through trusted `F::map`.
        unsafe { &*F::map(self.as_ptr()).cast::<MaybeUninit<F::Type>>() }
    }
}

impl<'a, T, F> Projectable<T, F> for &'a mut MaybeUninit<T>
where
    F: Field<T>,
    F::Type: Sized + 'a,
{
    type Target = &'a mut MaybeUninit<F::Type>;

    unsafe fn project(self) -> Self::Target {
        // SAFETY: Projecting through trusted `F::map`.
        unsafe {
            &mut *F::map(self.as_mut_ptr())
                .cast_mut()
                .cast::<MaybeUninit<F::Type>>()
        }
    }
}

#[macro_export]
macro_rules! project {
    ($a:expr => $b:ident) => {
        match $a {
            __expr => unsafe {
                $crate::projection::Projectable::<
                    _,
                    $crate::projection::FieldName<{ $crate::projection::field_name_hash(core::stringify!($b)) }>,
                >::project_with_check(__expr, |__check| {
                    let _ = __check.$b;
                })
            },
        }
    };
}

/// Additional information on a field of a struct regarding to pinning.
///
/// # Safety
/// `PinWrapper` must be layout-compatible with `&mut Self::Type`. If the field is pinned, then
/// it should be `Pin<&mut Self::Type>`, otherwise it should be `&mut Self::Type`.
///
/// This trait should not be implemented manually; instead, use the `#[derive(PinField)]` instead.
pub unsafe trait PinField<T>: Field<T> {
    /// The type when this field is projected from a `Pin<&mut Self::Base>`.
    type PinWrapper<'a, U: ?Sized + 'a>;

    /// The type when this field is projected from a `Pin<&mut MaybeUninit<Self::Base>>`.
    type PinMaybeUninitWrapper<'a, U: 'a>;
}

impl<'a, T, F> Projectable<T, F> for Pin<&'a mut T>
where
    F: PinField<T>,
    F::Type: 'a,
    T: HasField,
{
    type Target = F::PinWrapper<'a, F::Type>;

    unsafe fn project(self) -> Self::Target {
        // SAFETY: This pointer will not be moved out, and the resulting projection will be wrapped
        // with `Pin` back if the field is pinned.
        let inner = unsafe { Self::into_inner_unchecked(self) };
        // SAFETY: Project the pointer through raw pointer. Note that the `*mut _` cast is important
        // as otherwise the `&mut` to `*const` cast will go through `&` reference which will retag it.
        let ptr = unsafe { &mut *F::map(inner as *mut _).cast_mut() };
        // This is either a `Pin<&mut T>` or `&mut T`, both layout compatible with `&mut T`.
        // Use `transmute_copy` here because the compiler can't prove that `F::PinWrapper` is of
        // the same size.
        unsafe { core::mem::transmute_copy(&ptr) }
    }
}

impl<'a, T, F> Projectable<T, F> for Pin<&'a mut MaybeUninit<T>>
where
    F: PinField<T>,
    F::Type: Sized + 'a,
{
    type Target = F::PinMaybeUninitWrapper<'a, F::Type>;

    unsafe fn project(self) -> Self::Target {
        // SAFETY: This pointer will not be moved out, and the resulting projection will be wrapped
        // with `Pin` back if the field is pinned.
        let inner = unsafe { Self::into_inner_unchecked(self) };
        // SAFETY: Project the pointer through raw pointer.
        let ptr = unsafe { &mut *F::map(inner.as_mut_ptr()).cast_mut() };
        unsafe { core::mem::transmute_copy(&ptr) }
    }
}


#[doc(hidden)]
pub struct AlwaysUnpin<T>(T);
impl<T> Unpin for AlwaysUnpin<T> {}
