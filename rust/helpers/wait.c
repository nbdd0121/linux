// SPDX-License-Identifier: GPL-2.0

#include <linux/export.h>
#include <linux/wait.h>
#include "helpers.h"

__rust_helper void rust_helper_init_wait(struct wait_queue_entry *wq_entry)
{
	init_wait(wq_entry);
}
