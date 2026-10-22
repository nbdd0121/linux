// SPDX-License-Identifier: GPL-2.0

//! Small example demonstrating how to use [`drm::JobQueue`].

use kernel::dma_buf::{FenceContext, FenceContextOps};
use kernel::drm::{JobQueue, JobQueueOps, JobRunnable};
use kernel::prelude::*;
use kernel::str::CString;
use kernel::task::might_sleep;
use kernel::time::{delay::fsleep, Delta};

module! {
    type: RustJobqueueTester,
    name: "rust_jobqueue_tester",
    authors: ["Philipp Stanner"],
    description: "Rust minimal sample",
    license: "GPL",
}

// -- FenceContext data for external dependency fences -------------------------

#[pin_data]
struct DepFenceCtxData {}

impl DepFenceCtxData {
    fn new() -> impl PinInit<Self> {
        pin_init!(Self {})
    }
}

impl FenceContextOps for DepFenceCtxData {
    type FenceDataType = ();
}

// -- JobQueue data and ops ----------------------------------------------------

#[pin_data]
struct JqData {}

impl JqData {
    fn new() -> impl PinInit<Self> {
        pin_init!(Self {})
    }
}

struct JobPayload {}

impl Drop for JobPayload {
    fn drop(&mut self) {
        might_sleep();
    }
}

impl JobQueueOps for JqData {
    type JobDataType = JobPayload;

    fn run_job<'a>(&self, job: JobRunnable<'a, JobPayload>) {
        // Simulate GPU work completing after a short delay. In a real driver
        // this would submit work to the hardware ring.
        pr_info!("run_job: seqno {}\n", job.seqno);
    }
}

struct RustJobqueueTester {}

impl kernel::Module for RustJobqueueTester {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("Rust Jobqueue Tester (init)\n");
        pr_info!("Am I built-in? {}\n", !cfg!(MODULE));

        // A standalone FenceContext for creating external dependency fences.
        let dep_fctx = KBox::pin_init(
            FenceContext::new(
                0,
                c"rust-jq-tester",
                c"dep-timeline",
                DepFenceCtxData::new(),
            ),
            GFP_KERNEL,
        )?;

        let driver_name = CString::try_from_fmt(fmt!("rust-jobqueue-tester"))?;
        let timeline_name = CString::try_from_fmt(fmt!("rust-jq-tester-hw-timeline"))?;
        let jq = KBox::pin_init(
            JobQueue::new(0, driver_name, timeline_name, 1_000_000, JqData::new()),
            GFP_KERNEL,
        )?;

        pr_info!("Test 1: Test submitting two jobs without dependencies.\n");
        let job1 = jq.new_job(1, JobPayload {})?;
        let job2 = jq.new_job(1, JobPayload {})?;

        let fence1 = job1.submit();
        let fence2 = job2.submit();

        // Complete both jobs by seqno so their fences get signaled.
        jq.complete_jobs_up_to_seqno(fence2.seqno());

        // Give the submit worker a chance to run.
        fsleep(Delta::from_millis(100));

        if !fence1.is_signaled() || !fence2.is_signaled() {
            pr_info!("Test 1 failed.\n");
            return Err(EAGAIN);
        }
        pr_info!("Test 1 succeeded.\n");

        pr_info!("Test 2: Test submitting a job with already-fulfilled dependency.\n");
        let mut job3 = jq.new_job(1, JobPayload {})?;
        job3.add_dependency(&fence1)?;

        let fence3 = job3.submit();
        jq.complete_jobs_up_to_seqno(fence3.seqno());

        // Give the submit worker a chance to run.
        fsleep(Delta::from_millis(100));

        if !fence3.is_signaled() {
            pr_info!("Test 2 failed.\n");
            return Err(EAGAIN);
        }
        pr_info!("Test 2 succeeded.\n");

        pr_info!("Test 3: Test that a job with unfulfilled dependency does not get run.\n");
        let unsignaled_alloc = dep_fctx.new_fence_allocation(())?;
        let unsignaled_fence = unsignaled_alloc.new_fence();

        let mut job4 = jq.new_job(1, JobPayload {})?;
        job4.add_dependency(unsignaled_fence.as_fence())?;

        let blocked_job_fence = job4.submit();

        // Give the submit worker a chance to run (it should NOT run job4).
        fsleep(Delta::from_millis(100));

        if blocked_job_fence.is_signaled() {
            pr_info!("Test 3 failed.\n");
            return Err(EAGAIN);
        }
        pr_info!("Test 3 succeeded.\n");

        pr_info!("Test 4: Test whether Test 3's blocked job can be unblocked.\n");
        unsignaled_fence.signal(Ok(()));

        // Give the submit worker a chance to run the now-unblocked job.
        fsleep(Delta::from_millis(500));

        // Complete it.
        jq.complete_jobs_up_to_seqno(blocked_job_fence.seqno());
        fsleep(Delta::from_millis(100));

        if !blocked_job_fence.is_signaled() {
            pr_info!("Test 4 failed.\n");
            return Err(EAGAIN);
        }
        pr_info!("Test 4 succeeded.\n");

        pr_info!("Test 5: Submit unblocked jobs, then a blocked one, then unblocked ones.\n");
        let job1 = jq.new_job(1, JobPayload {})?;
        let job2 = jq.new_job(1, JobPayload {})?;
        let mut job3 = jq.new_job(1, JobPayload {})?;
        let job4 = jq.new_job(1, JobPayload {})?;
        let job5 = jq.new_job(1, JobPayload {})?;

        let unsignaled_alloc1 = dep_fctx.new_fence_allocation(())?;
        let unsignaled_fence1 = unsignaled_alloc1.new_fence();
        let unsignaled_alloc2 = dep_fctx.new_fence_allocation(())?;
        let unsignaled_fence2 = unsignaled_alloc2.new_fence();
        let unsignaled_alloc3 = dep_fctx.new_fence_allocation(())?;
        let unsignaled_fence3 = unsignaled_alloc3.new_fence();

        job3.add_dependency(unsignaled_fence1.as_fence())?;
        job3.add_dependency(unsignaled_fence2.as_fence())?;
        job3.add_dependency(unsignaled_fence3.as_fence())?;

        let fence1 = job1.submit();
        let fence2 = job2.submit();
        let fence3 = job3.submit();

        // Complete unblocked jobs 1 and 2.
        jq.complete_jobs_up_to_seqno(fence2.seqno());
        fsleep(Delta::from_millis(100));

        if fence3.is_signaled() || !fence1.is_signaled() || !fence2.is_signaled() {
            pr_info!("Test 5 failed (phase 1).\n");
            return Err(EAGAIN);
        }

        // Signal two of three dependencies — job3 should still be blocked.
        unsignaled_fence1.signal(Ok(()));
        unsignaled_fence3.signal(Ok(()));
        fsleep(Delta::from_millis(200));

        if fence3.is_signaled() {
            pr_info!("Test 5 failed (phase 2).\n");
            return Err(EAGAIN);
        }

        // Signal the last dependency — job3 should now be runnable.
        unsignaled_fence2.signal(Ok(()));
        fsleep(Delta::from_millis(500));

        // Complete job3 and the remaining jobs.
        jq.complete_jobs_up_to_seqno(fence3.seqno());
        fsleep(Delta::from_millis(100));

        if !fence3.is_signaled() {
            pr_info!("Test 5 failed (phase 3).\n");
            return Err(EAGAIN);
        }

        let fence4 = job4.submit();
        let fence5 = job5.submit();
        jq.complete_jobs_up_to_seqno(fence5.seqno());
        fsleep(Delta::from_millis(100));

        if !fence4.is_signaled() || !fence5.is_signaled() {
            pr_info!("Test 5 failed (phase 4).\n");
            return Err(EAGAIN);
        }
        pr_info!("Test 5 succeeded.\n");

        Ok(RustJobqueueTester {})
    }
}

impl Drop for RustJobqueueTester {
    fn drop(&mut self) {
        pr_info!("Rust Jobqueue Tester (exit)\n");
    }
}
