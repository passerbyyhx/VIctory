//! A CPU probe between submission and completion on the host's real direct queue.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Graphics::Direct3D12::{D3D12_FENCE_FLAG_NONE, ID3D12Fence};
use windows::Win32::System::Threading::WaitForSingleObject;

use super::{Host, HostList};

pub(super) struct Probe<'a> {
    gate: ID3D12Fence,
    inspect: &'a mut dyn FnMut(),
    observed: Option<(u64, u64, u64, u64)>,
}

impl Probe<'_> {
    pub(super) fn hold_queue(&self, host: &Host) {
        // SAFETY: host.execute_lists holds the submission lock. Only our CPU signals this gate.
        unsafe { host.queue.Wait(&self.gate, 1) }.expect("queue the test gate");
    }

    pub(super) fn observe_and_wait(
        &mut self,
        host: &Host,
        target: u64,
        registered: windows::core::Result<()>,
    ) {
        // SAFETY: this fence belongs to the live host. The target is from the current submission.
        let before = unsafe { host.fence.GetCompletedValue() };
        if registered.is_ok() && before < target {
            // The caller captures probe panics so they cannot prevent opening the gate.
            (self.inspect)();
        }
        // SAFETY: still on the submitting thread, before the CPU opens the gate.
        let after_probe = unsafe { host.fence.GetCompletedValue() };
        // SAFETY: open the gate before waiting, including when the original signal or
        // event registration failed. In that case arrange a fresh signal after the list.
        let released = unsafe { self.gate.Signal(1) };
        let ready = released.and_then(|()| {
            if registered.is_ok() {
                return Ok(());
            }
            // SAFETY: the queue, fence and event remain under the host submission lock.
            unsafe {
                host.queue.Signal(&host.fence, target)?;
                host.fence.SetEventOnCompletion(target, host.event)
            }
        });
        // SAFETY: the host event was registered for this target before the wait.
        let signalled =
            ready.is_ok() && unsafe { WaitForSingleObject(host.event, 15_000) } == WAIT_OBJECT_0;
        // SAFETY: read the actual completion value while this is still the only submission.
        let after = unsafe { host.fence.GetCompletedValue() };
        assert_ne!(
            after,
            u64::MAX,
            "device removed during the gated submission"
        );
        if after < target {
            // SAFETY: a removed device has stopped using the fixture. A live device that
            // did not drain may still use it; unwinding would release the caller's resources.
            unsafe { host.device.GetDeviceRemovedReason() }.expect("test device removed");
            eprintln!("host fence did not reach {target} (completed {after}, gate {ready:?})");
            std::process::abort();
        }
        assert!(
            signalled,
            "host event failed to report completion of {target}: {ready:?}"
        );
        registered.expect("register the host submission fence");
        self.observed = Some((before, after_probe, target, after));
    }
}

/// The probe may inspect its own CPU state. It must not submit work or wait for the GPU.
pub(crate) fn while_pending<T>(
    host: &Host,
    list: &HostList,
    probe: impl FnOnce() -> T,
) -> (T, u64, u64) {
    // SAFETY: create an initially unsignalled fence on the host device.
    let gate =
        unsafe { host.device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }.expect("create the CPU gate");
    let mut probe = Some(probe);
    let mut result = None;
    let mut inspect = || {
        result = Some(catch_unwind(AssertUnwindSafe(|| {
            probe.take().expect("observe one submission")()
        })));
    };
    let mut hook = Probe {
        gate,
        inspect: &mut inspect,
        observed: None,
    };
    host.execute_lists(&[list], Some(&mut hook));
    let (before, after_probe, target, after) =
        hook.observed.expect("host submitted and finished the list");
    eprintln!(
        "host fence: before={before}, after_probe={after_probe}, target={target}, after={after}"
    );
    assert!(
        before < target,
        "submission already completed before the probe"
    );
    assert!(
        after_probe < target,
        "submission completed while the gate was closed"
    );
    match result.expect("probe ran") {
        Ok(value) => (value, before, after),
        Err(panic) => resume_unwind(panic),
    }
}
