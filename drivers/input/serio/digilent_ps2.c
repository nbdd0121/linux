// SPDX-License-Identifier: GPL-2.0-or-later

#include <linux/module.h>
#include <linux/serio.h>
#include <linux/interrupt.h>
#include <linux/errno.h>
#include <linux/slab.h>
#include <linux/list.h>
#include <linux/io.h>
#include <linux/of_address.h>
#include <linux/of_device.h>
#include <linux/of_irq.h>
#include <linux/of_platform.h>

#define DRIVER_NAME		"digilent_ps2"

/* Register offsets for the digilent_ps2 device */
#define DPS2_SRST_OFFSET	0x00000000 /* Software Reset register */
#define DPS2_STATUS_OFFSET	0x00000004 /* Status register */
#define DPS2_RX_DATA_OFFSET	0x00000008 /* Receive Data register */
#define DPS2_TX_DATA_OFFSET	0x0000000C /* Transmit Data register */
#define DPS2_GIER_OFFSET	0x00000010 /* Global Interrupt Enable reg */
#define DPS2_IPISR_OFFSET	0x00000014 /* Interrupt Status register */
#define DPS2_IPIER_OFFSET	0x00000018 /* Interrupt Enable register */

/* Reset Register Bit Definitions */
#define DPS2_SRST_RESET		0x0000000A /* Software Reset  */

/* Status Register Bit Positions */
#define DPS2_STATUS_TX_FULL     0x00000001 /* Transmit Full */
#define DPS2_STATUS_TX_EMPTY    0x00000002 /* Transmit Empty */
#define DPS2_STATUS_RX_FULL     0x00000004 /* Receive Full */
#define DPS2_STATUS_RX_EMPTY    0x00000008 /* Receive Empty */

/*
 * Bit definitions for ISR/IER registers. Both the registers have the same bit
 * definitions and are only defined once.
 */
#define DPS2_IPIXR_TX_NOACK	0x40000000 /* Transmit No ACK Interrupt */
#define DPS2_IPIXR_TX_ACK	0x20000000 /* Transmit ACK (Data) Interrupt */
#define DPS2_IPIXR_RX_OVF	0x10000000 /* Receive Overflow Interrupt */
#define DPS2_IPIXR_RX_ERR	0x08000000 /* Receive Error Interrupt */
#define DPS2_IPIXR_RX_FULL	0x04000000 /* Receive Data Interrupt */

/* Mask for all the Transmit Interrupts */
#define DPS2_IPIXR_TX_ALL	(DPS2_IPIXR_TX_NOACK | DPS2_IPIXR_TX_ACK)

/* Mask for all the Receive Interrupts */
#define DPS2_IPIXR_RX_ALL	(DPS2_IPIXR_RX_OVF | DPS2_IPIXR_RX_ERR |  \
				 DPS2_IPIXR_RX_FULL)

/* Mask for all the Interrupts */
#define DPS2_IPIXR_ALL		(DPS2_IPIXR_TX_ALL | DPS2_IPIXR_RX_ALL)

/* Global Interrupt Enable mask */
#define DPS2_GIER_GIE_MASK	0x00000001

struct digilent_ps2data {
	int irq;
	spinlock_t lock;
	void __iomem *base_address;	/* virt. address of control registers */
	unsigned int flags;
	struct serio *serio;		/* serio */
	struct device *dev;
};

/************************************/
/* XPS PS/2 data transmission calls */
/************************************/

/**
 * digilent_ps2_recv() - attempts to receive a byte from the PS/2 port.
 * @drvdata:	pointer to ps2 device private data structure
 * @byte:	address where the read data will be copied
 *
 * If there is any data available in the PS/2 receiver, this functions reads
 * the data, otherwise it returns error.
 */
static int digilent_ps2_recv(struct digilent_ps2data *drvdata, u8 *byte)
{
	u32 sr;
	int status = -1;

	/* If there is data available in the PS/2 receiver, read it */
	sr = readl(drvdata->base_address + DPS2_STATUS_OFFSET);
	if (!(sr & DPS2_STATUS_RX_EMPTY)) {
		*byte = readl(drvdata->base_address + DPS2_RX_DATA_OFFSET);
		status = 0;
	}

	return status;
}

/*********************/
/* Interrupt handler */
/*********************/
static irqreturn_t digilent_ps2_interrupt(int irq, void *dev_id)
{
	struct digilent_ps2data *drvdata = dev_id;
	u32 intr_sr;
	u8 c;
	int status;

	/* Get the PS/2 interrupts and clear them */
	intr_sr = readl(drvdata->base_address + DPS2_IPISR_OFFSET);
	writel(intr_sr, drvdata->base_address + DPS2_IPISR_OFFSET);

	/* Check which interrupt is active */
	if (intr_sr & DPS2_IPIXR_RX_OVF)
		dev_warn(drvdata->dev, "receive overrun error\n");

	if (intr_sr & DPS2_IPIXR_RX_ERR)
		drvdata->flags |= SERIO_PARITY;

	if (intr_sr & (DPS2_IPIXR_TX_NOACK))
		drvdata->flags |= SERIO_TIMEOUT;

	if (intr_sr & DPS2_IPIXR_RX_FULL) {
		status = digilent_ps2_recv(drvdata, &c);

		/* Error, if a byte is not received */
		if (status) {
			dev_err(drvdata->dev,
				"wrong rcvd byte count (%d)\n", status);
		} else {
			serio_interrupt(drvdata->serio, c, drvdata->flags);
			drvdata->flags = 0;
		}
	}

	return IRQ_HANDLED;
}

/*******************/
/* serio callbacks */
/*******************/

/**
 * sdigilent_ps2_write() - sends a byte out through the PS/2 port.
 * @pserio:	pointer to the serio structure of the PS/2 port
 * @c:		data that needs to be written to the PS/2 port
 *
 * This function checks if the PS/2 transmitter is not full and sends a byte.
 * Otherwise it returns error. Transmission fails only when nothing is connected
 * to the PS/2 port. Thats why, we do not try to resend the data in case of a
 * failure.
 */
static int sdigilent_ps2_write(struct serio *pserio, unsigned char c)
{
	struct digilent_ps2data *drvdata = pserio->port_data;
	unsigned long flags;
	u32 sr;
	int status = -1;

	spin_lock_irqsave(&drvdata->lock, flags);

	/* If the PS/2 transmitter is not full send a byte of data */
	sr = readl(drvdata->base_address + DPS2_STATUS_OFFSET);
	if (!(sr & DPS2_STATUS_TX_FULL)) {
		writel(c, drvdata->base_address + DPS2_TX_DATA_OFFSET);
		status = 0;
	}

	spin_unlock_irqrestore(&drvdata->lock, flags);

	return status;
}

/**
 * sdigilent_ps2_open() - called when a port is opened by the higher layer.
 * @pserio:	pointer to the serio structure of the PS/2 device
 *
 * This function requests irq and enables interrupts for the PS/2 device.
 */
static int sdigilent_ps2_open(struct serio *pserio)
{
	struct digilent_ps2data *drvdata = pserio->port_data;
	int error;
	u8 c;

	error = request_irq(drvdata->irq, &digilent_ps2_interrupt, 0,
				DRIVER_NAME, drvdata);
	if (error) {
		dev_err(drvdata->dev,
			"Couldn't allocate interrupt %d\n", drvdata->irq);
		return error;
	}

	/* start reception by enabling the interrupts */
	writel(DPS2_GIER_GIE_MASK, drvdata->base_address + DPS2_GIER_OFFSET);
	writel(DPS2_IPIXR_RX_ALL, drvdata->base_address + DPS2_IPIER_OFFSET);
	(void)digilent_ps2_recv(drvdata, &c);

	return 0;		/* success */
}

/**
 * sdigilent_ps2_close() - frees the interrupt.
 * @pserio:	pointer to the serio structure of the PS/2 device
 *
 * This function frees the irq and disables interrupts for the PS/2 device.
 */
static void sdigilent_ps2_close(struct serio *pserio)
{
	struct digilent_ps2data *drvdata = pserio->port_data;

	/* Disable the PS2 interrupts */
	writel(0x00, drvdata->base_address + DPS2_GIER_OFFSET);
	writel(0x00, drvdata->base_address + DPS2_IPIER_OFFSET);
	free_irq(drvdata->irq, drvdata);
}

/**
 * digilent_ps2_of_probe - probe method for the PS/2 device.
 * @of_dev:	pointer to OF device structure
 * @match:	pointer to the structure used for matching a device
 *
 * This function probes the PS/2 device in the device tree.
 * It initializes the driver data structure and the hardware.
 * It returns 0, if the driver is bound to the PS/2 device, or a negative
 * value if there is an error.
 */
static int digilent_ps2_of_probe(struct platform_device *ofdev)
{
	struct resource r_mem; /* IO mem resources */
	struct digilent_ps2data *drvdata;
	struct serio *serio;
	struct device *dev = &ofdev->dev;
	resource_size_t remap_size, phys_addr;
	unsigned int irq;
	int error;

	dev_info(dev, "Device Tree Probing \'%pOFn\'\n", dev->of_node);

	/* Get iospace for the device */
	error = of_address_to_resource(dev->of_node, 0, &r_mem);
	if (error) {
		dev_err(dev, "invalid address\n");
		return error;
	}

	/* Get IRQ for the device */
	irq = irq_of_parse_and_map(dev->of_node, 0);
	if (!irq) {
		dev_err(dev, "no IRQ found\n");
		return -ENODEV;
	}

	drvdata = kzalloc(sizeof(struct digilent_ps2data), GFP_KERNEL);
	serio = kzalloc(sizeof(struct serio), GFP_KERNEL);
	if (!drvdata || !serio) {
		error = -ENOMEM;
		goto failed1;
	}

	spin_lock_init(&drvdata->lock);
	drvdata->irq = irq;
	drvdata->serio = serio;
	drvdata->dev = dev;

	phys_addr = r_mem.start;
	remap_size = resource_size(&r_mem);
	if (!request_mem_region(phys_addr, remap_size, DRIVER_NAME)) {
		dev_err(dev, "Couldn't lock memory region at 0x%08llX\n",
			(unsigned long long)phys_addr);
		error = -EBUSY;
		goto failed1;
	}

	/* Fill in configuration data and add them to the list */
	drvdata->base_address = ioremap(phys_addr, remap_size);
	if (drvdata->base_address == NULL) {
		dev_err(dev, "Couldn't ioremap memory at 0x%08llX\n",
			(unsigned long long)phys_addr);
		error = -EFAULT;
		goto failed2;
	}

	/* Disable all the interrupts, just in case */
	writel(0, drvdata->base_address + DPS2_IPIER_OFFSET);

	/*
	 * Reset the PS2 device and abort any current transaction,
	 * to make sure we have the PS2 in a good state.
	 */
	writel(DPS2_SRST_RESET, drvdata->base_address + DPS2_SRST_OFFSET);

	dev_info(dev, "Digilent PS2 at 0x%08llX mapped to 0x%p, irq=%d\n",
		 (unsigned long long)phys_addr, drvdata->base_address,
		 drvdata->irq);

	serio->id.type = SERIO_8042;
	serio->write = sdigilent_ps2_write;
	serio->open = sdigilent_ps2_open;
	serio->close = sdigilent_ps2_close;
	serio->port_data = drvdata;
	serio->dev.parent = dev;
	snprintf(serio->name, sizeof(serio->name),
		 "Digilent XPS PS/2 at %08llX", (unsigned long long)phys_addr);
	snprintf(serio->phys, sizeof(serio->phys),
		 "digilent_ps2/serio at %08llX", (unsigned long long)phys_addr);

	serio_register_port(serio);

	platform_set_drvdata(ofdev, drvdata);
	return 0;		/* success */

failed2:
	release_mem_region(phys_addr, remap_size);
failed1:
	kfree(serio);
	kfree(drvdata);

	return error;
}

/**
 * digilent_ps2_of_remove - unbinds the driver from the PS/2 device.
 * @of_dev:	pointer to OF device structure
 *
 * This function is called if a device is physically removed from the system or
 * if the driver module is being unloaded. It frees any resources allocated to
 * the device.
 */
static int digilent_ps2_of_remove(struct platform_device *of_dev)
{
	struct digilent_ps2data *drvdata = platform_get_drvdata(of_dev);
	struct resource r_mem; /* IO mem resources */

	serio_unregister_port(drvdata->serio);
	iounmap(drvdata->base_address);

	/* Get iospace of the device */
	if (of_address_to_resource(of_dev->dev.of_node, 0, &r_mem))
		dev_err(drvdata->dev, "invalid address\n");
	else
		release_mem_region(r_mem.start, resource_size(&r_mem));

	kfree(drvdata);

	return 0;
}

/* Match table for of_platform binding */
static const struct of_device_id digilent_ps2_of_match[] = {
	{ .compatible = "digilent,axi-ps2-1.0", },
	{ /* end of list */ },
};
MODULE_DEVICE_TABLE(of, digilent_ps2_of_match);

static struct platform_driver digilent_ps2_of_driver = {
	.driver = {
		.name = DRIVER_NAME,
		.of_match_table = digilent_ps2_of_match,
	},
	.probe		= digilent_ps2_of_probe,
	.remove		= digilent_ps2_of_remove,
};
module_platform_driver(digilent_ps2_of_driver);

MODULE_AUTHOR("Gary Guo");
MODULE_DESCRIPTION("Digilent PS/2 driver");
MODULE_LICENSE("GPL");

