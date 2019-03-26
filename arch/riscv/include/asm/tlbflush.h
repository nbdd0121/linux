/* SPDX-License-Identifier: GPL-2.0 */
/*
 * Copyright (C) 2009 Chen Liqin <liqin.chen@sunplusct.com>
 * Copyright (C) 2012 Regents of the University of California
 * Copyright (C) 2019 Gary Guo, University of Cambridge
 */

#ifndef _ASM_RISCV_TLBFLUSH_H
#define _ASM_RISCV_TLBFLUSH_H

#include <linux/mm_types.h>

/*
 * Flush entire local TLB.  'sfence.vma' implicitly fences with the instruction
 * cache as well, so a 'fence.i' is not necessary.
 */
static inline void local_flush_tlb_all(void)
{
	__asm__ __volatile__ ("sfence.vma" : : : "memory");
}

static inline void local_flush_tlb_mm(struct mm_struct *mm)
{
	/* Flush ASID 0 so that global mappings are not affected */
	__asm__ __volatile__ ("sfence.vma x0, %0" : : "r" (0) : "memory");
}

static inline void local_flush_tlb_page(struct vm_area_struct *vma,
	unsigned long addr)
{
	__asm__ __volatile__ ("sfence.vma %0, %1"
			      : : "r" (addr), "r" (0)
			      : "memory");
}

static inline void local_flush_tlb_kernel_page(unsigned long addr)
{
	__asm__ __volatile__ ("sfence.vma %0" : : "r" (addr) : "memory");
}

void local_flush_tlb_range(struct vm_area_struct *vma, unsigned long start,
	unsigned long end);
void local_flush_tlb_kernel_range(unsigned long start, unsigned long end);

#ifdef CONFIG_SMP

void flush_tlb_all(void);
void flush_tlb_mm(struct mm_struct *mm);
void flush_tlb_page(struct vm_area_struct *vma, unsigned long addr);
void flush_tlb_range(struct vm_area_struct *vma, unsigned long start,
	unsigned long end);
void flush_tlb_kernel_range(unsigned long start, unsigned long end);

#else /* CONFIG_SMP */

#define flush_tlb_all() local_flush_tlb_all()
#define flush_tlb_mm(mm) local_flush_tlb_mm(mm)
#define flush_tlb_page(vma, addr) local_flush_tlb_page(vma, addr)
#define flush_tlb_range(vma, start, end) local_flush_tlb_range(vma, start, end)
#define flush_tlb_kernel_range(start, end) \
	local_flush_tlb_kernel_range(start, end)

#endif /* CONFIG_SMP */

#endif /* _ASM_RISCV_TLBFLUSH_H */
