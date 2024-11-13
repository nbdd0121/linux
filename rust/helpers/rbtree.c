// SPDX-License-Identifier: GPL-2.0

#include <linux/rbtree.h>
#include "helpers.h"

__rust_helper void rust_helper_rb_link_node(struct rb_node *node,
					    struct rb_node *parent,
					    struct rb_node **rb_link)
{
	rb_link_node(node, parent, rb_link);
}
