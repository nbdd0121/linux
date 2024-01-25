//!

#![no_std]

macro_rules! alias {
    ($($name:ident = $ty:ty;)*) => {$(
        #[allow(non_camel_case_types, missing_docs)]
        pub type $name = $ty;

        // Check size compatibility with libcore.
        const _: () = assert!(
            core::mem::size_of::<$name>() == core::mem::size_of::<core::ffi::$name>()
        );
    )*}
}

alias! {
    c_char = u8;
    c_schar = i8;
    c_uchar = u8;

    c_short = i16;
    c_ushort = u16;

    c_int = i32;
    c_uint = u32;

    // Mandated by the kernel ABI.
    c_long = isize;
    c_ulong = usize;

    c_longlong = i64;
    c_ulonglong = u64;
}

pub use core::ffi::c_void;
