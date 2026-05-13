// SPDX-License-Identifier: GPL-2.0

use kernel::{
    auxiliary,
    debugfs,
    device::Core,
    devres::Devres,
    dma::{
        Device,
        DmaMask, //
    },
    pci::{
        self,
        Class,
        ClassMask,
        Vendor, //
    },
    prelude::*,
    sizes::SZ_16M,
    sync::{
        atomic::{
            Atomic,
            Relaxed, //
        },
        Arc,
    }, //
};

use crate::gpu::Gpu;

/// Counter for generating unique auxiliary device IDs.
static AUXILIARY_ID_COUNTER: Atomic<u32> = Atomic::new(0);

#[pin_data]
pub(crate) struct NovaCore<'bound> {
    #[pin]
    pub(crate) gpu: Gpu,
    _reg: Devres<auxiliary::Registration<()>>,
    debugfs: &'bound debugfs::Dir,
}

pub(crate) struct NovaCoreDriver<'module> {
    pub(crate) debugfs: &'module debugfs::Dir,
}

const BAR0_SIZE: usize = SZ_16M;

// For now we only support Ampere which can use up to 47-bit DMA addresses.
//
// TODO: Add an abstraction for this to support newer GPUs which may support
// larger DMA addresses. Limiting these GPUs to smaller address widths won't
// have any adverse affects, unless installed on systems which require larger
// DMA addresses. These systems should be quite rare.
const GPU_DMA_BITS: u32 = 47;

pub(crate) type Bar0 = pci::Bar<BAR0_SIZE>;

kernel::pci_device_table!(
    PCI_TABLE,
    MODULE_PCI_TABLE,
    <NovaCoreDriver<'_> as pci::DriverNew>::IdInfo,
    [
        // Modern NVIDIA GPUs will show up as either VGA or 3D controllers.
        (
            pci::DeviceId::from_class_and_vendor(
                Class::DISPLAY_VGA,
                ClassMask::ClassSubclass,
                Vendor::NVIDIA
            ),
            ()
        ),
        (
            pci::DeviceId::from_class_and_vendor(
                Class::DISPLAY_3D,
                ClassMask::ClassSubclass,
                Vendor::NVIDIA
            ),
            ()
        ),
    ]
);

impl pci::DriverNew for NovaCoreDriver<'_> {
    type Data<'bound>
        = NovaCore<'bound>
    where
        Self: 'bound;

    type IdInfo = ();
    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;

    fn probe<'bound>(
        &'bound self,
        pdev: &'bound pci::Device<Core>,
        _info: &Self::IdInfo,
    ) -> impl PinInit<NovaCore<'bound>, Error> {
        pin_init::pin_init_scope(move || {
            dev_dbg!(pdev, "Probe Nova Core GPU driver.\n");

            pdev.enable_device_mem()?;
            pdev.set_master();

            // SAFETY: No concurrent DMA allocations or mappings can be made because
            // the device is still being probed and therefore isn't being used by
            // other threads of execution.
            unsafe { pdev.dma_set_mask_and_coherent(DmaMask::new::<GPU_DMA_BITS>())? };

            let bar = Arc::pin_init(
                pdev.iomap_region_sized::<BAR0_SIZE>(0, c"nova-core/bar0"),
                GFP_KERNEL,
            )?;

            Ok(try_pin_init!(NovaCore {
                gpu <- Gpu::new(self, pdev, bar.clone(), bar.access(pdev.as_ref())?),
                _reg: auxiliary::Registration::new(
                    pdev.as_ref(),
                    c"nova-drm",
                    // TODO[XARR]: Use XArray or perhaps IDA for proper ID allocation/recycling. For
                    // now, use a simple atomic counter that never recycles IDs.
                    AUXILIARY_ID_COUNTER.fetch_add(1, Relaxed),
                    crate::MODULE_NAME,
                    (),
                )?,
                debugfs: self.debugfs,
            }))
        })
    }

    fn unbind(&self, pdev: &pci::Device<Core>, this: Pin<&NovaCore<'_>>) {
        this.gpu.unbind(pdev.as_ref());
    }
}
