/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Copyright (C) 2012 Regents of the University of California
 * Copyright (C) 2019 Gary Guo, University of Cambridge
 */


#ifndef _ASM_RISCV_MMU_H
#define _ASM_RISCV_MMU_H

#ifndef __ASSEMBLY__

#if __riscv_xlen == 32
#define ASIDMAX	9
#else
#define ASIDMAX	16
#endif

#define ASIDMAX_MASK GENMASK(ASIDMAX - 1, 0)
#define ASID(mm) (atomic_long_read(&(mm)->context.asid) & ASIDMAX_MASK)

typedef struct {
#ifndef CONFIG_MMU
	unsigned long	end_brk;
#endif
	/*
	 * ASID assigned to this MM context.
	 * - If ASID is disabled or not yet assigned to this MM context, it
	 *   contains 0.
	 * - Otherwise it may either contains a valid ASID for this generation
	 *   or a stale ASID for previous generation.
	 */
	atomic_long_t asid;
	void *vdso;
#ifdef CONFIG_SMP
	/* A local icache flush is needed before user execution can resume. */
	cpumask_t icache_stale_mask;
	/* A mask indicating which harts have accessed this MM context. */
	cpumask_t cache_mask;
#endif
} mm_context_t;

#endif /* __ASSEMBLY__ */

#endif /* _ASM_RISCV_MMU_H */
