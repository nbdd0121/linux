// SPDX-License-Identifier: GPL-2.0

//! Rust RCU sample.

use kernel::prelude::*;
use kernel::rcu_mutex_init;
use kernel::sync::rcu_mutex::{RcuField, RcuMutex};
use kernel::project;

module! {
    type: RustRcu,
    name: "rust_rcu",
    author: "Rust for Linux Contributors",
    description: "Rust RCU sample",
    license: "GPL",
}

struct RustRcu;

struct Rcu;

#[derive(kernel::projection::Field, RcuField)]
struct Test {
    a: Rcu,
    b: i32,
}

impl kernel::Module for RustRcu {
    fn init(_name: &'static CStr, _module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust RCU sample (init)\n");

        // Test mutexes.
        {
            // SAFETY: `init` is called below.
            let mut data = Pin::from(Box::try_new(unsafe { RcuMutex::new(Test {
                a: Rcu,
                b: 0
            }) })?);
            rcu_mutex_init!(data.as_mut(), "RustRcu::init::data1");
            let mut guard = data.lock();
            // Use projection to get a mutable reference
            *project!(&mut guard => b) = 10;
            let _: &Rcu = project!(&mut guard => a); // This will only give an immutable reference
            let _: &Rcu = project!(&*data => a); // Still able to get an immutable reference while mutex is locked.
            drop(guard);
            // Read can be done directly
            pr_info!("Value: {}\n", data.lock().b);
        }

        Ok(RustRcu)
    }
}

impl Drop for RustRcu {
    fn drop(&mut self) {
        pr_info!("Rust RCU sample (exit)\n");
    }
}
