// SPDX-License-Identifier: GPL-2.0

#include <kunit/test-bug.h>
#include <linux/export.h>
#include "helpers.h"

__rust_helper struct kunit *rust_helper_kunit_get_current_test(void)
{
	return kunit_get_current_test();
}
