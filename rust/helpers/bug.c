// SPDX-License-Identifier: GPL-2.0

#include <linux/bug.h>
#include "helpers.h"

__rust_helper __noreturn void rust_helper_BUG(void)
{
	BUG();
}
