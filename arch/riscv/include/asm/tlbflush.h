/* SPDX-License-Identifier: GPL-2.0-only */
/*
 * Copyright (C) 2009 Chen Liqin <liqin.chen@sunplusct.com>
 * Copyright (C) 2012 Regents of the University of California
 */

#ifndef _ASM_RISCV_TLBFLUSH_H
#define _ASM_RISCV_TLBFLUSH_H

#include <linux/mm_types.h>
#include <asm/smp.h>

#ifdef CONFIG_MMU
static inline void local_flush_tlb_all(void)
{
	__asm__ __volatile__ ("sfence.vma" : : : "memory");
}

/*
 * Flush one MM context.
 *
 * - If ASID is not enabled, then this flushes ASID 0, which is correct as we
 *   don't need ordering to global mappings.
 *
 * - If ASID is not assigned or is stale, then we don't need flush at all, so
 *   we can safely flush any ASID.
 *
 * - Otherwise we are flushing the correct ASID.
 */
static inline void local_flush_tlb_mm(struct mm_struct *mm)
{
	unsigned long asid = ASID(mm);
	__asm__ __volatile__ ("sfence.vma x0, %0" : : "r" (asid) : "memory");
}

/* Flush one page from local TLB */
static inline void local_flush_tlb_page(struct vm_area_struct *vma,
                                        unsigned long addr)
{
	unsigned long asid = ASID(vma->vm_mm);
	__asm__ __volatile__ ("sfence.vma %0, %1"
			      : : "r" (addr), "r" (asid)
			      : "memory");
}

static inline void local_flush_tlb_kernel_page(unsigned long addr)
{
	__asm__ __volatile__ ("sfence.vma %0" : : "r" (addr) : "memory");
}
#else /* CONFIG_MMU */
#define local_flush_tlb_all()			do { } while (0)
#define local_flush_tlb_page(addr)		do { } while (0)
#endif /* CONFIG_MMU */

#if defined(CONFIG_SMP) && defined(CONFIG_MMU)
void flush_tlb_all(void);
void flush_tlb_mm(struct mm_struct *mm);
void flush_tlb_page(struct vm_area_struct *vma, unsigned long addr);
void flush_tlb_range(struct vm_area_struct *vma, unsigned long start,
		     unsigned long end);
void flush_tlb_kernel_range(unsigned long start, unsigned long end);
#else /* CONFIG_SMP && CONFIG_MMU */

#define flush_tlb_all() local_flush_tlb_all()
#define flush_tlb_mm(mm) local_flush_tlb_mm(mm)
#define flush_tlb_page(vma, addr) local_flush_tlb_page(vma, addr)

static inline void flush_tlb_range(struct vm_area_struct *vma,
		unsigned long start, unsigned long end)
{
	if (end - start > PAGE_SIZE) {
		local_flush_tlb_mm(vma->vm_mm);
		return;
	}

	flush_tlb_page(vma, start);
}

/* Flush a range of kernel pages */
static inline void flush_tlb_kernel_range(unsigned long start,
	unsigned long end)
{
	if (end - start > PAGE_SIZE) {
		local_flush_tlb_all();
		return;
	}

	flush_tlb_kernel_page(start);
}

#endif /* !CONFIG_SMP || !CONFIG_MMU */

#endif /* _ASM_RISCV_TLBFLUSH_H */
