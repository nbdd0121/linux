// SPDX-License-Identifier: GPL-2.0
/*
 * Copyright (C) 2019 Gary Guo, University of Cambridge
 */

#include <linux/mm.h>
#include <asm/sbi.h>

#define SFENCE_VMA_FLUSH_ALL ((unsigned long) -1)

/*
 * This controls the maximum amount of page-level sfence.vma that the kernel
 * can issue when the kernel needs to flush a range from the TLB.  If the size
 * of range goes beyond this threshold, a full sfence.vma is issued.
 *
 * Increase this number can negatively impact performance on implementations
 * where sfence.vma's address operand is ignored and always perform a global
 * TLB flush.  On the other hand, implementations with page-level TLB flush
 * support can benefit from a larger number.
 */
static unsigned long tlbi_range_threshold = PAGE_SIZE;

static int __init setup_tlbi_max_ops(char *str)
{
	int value = 0;

	get_option(&str, &value);

	/*
	 * This value cannot be greater or equal to PTRS_PER_PTE, as we need
	 * to full flush for any non-leaf page table change. The value has also
	 * be at least 1.
	 */
	if (value >= PTRS_PER_PTE || value < 1)
		return -EINVAL;

	tlbi_range_threshold = value * PAGE_SIZE;
	return 0;
}
early_param("tlbi_max_ops", setup_tlbi_max_ops);

void local_flush_tlb_range(struct vm_area_struct *vma, unsigned long start,
			   unsigned long end)
{
	if (end - start > tlbi_range_threshold) {
		local_flush_tlb_mm(vma->vm_mm);
		return;
	}

	while (start < end) {
		__asm__ __volatile__ ("sfence.vma %0, %1"
				      : : "r" (start), "r" (0)
				      : "memory");
		start += PAGE_SIZE;
	}
}

void local_flush_tlb_kernel_range(unsigned long start, unsigned long end)
{
	if (end - start > tlbi_range_threshold) {
		local_flush_tlb_all();
		return;
	}

	while (start < end) {
		__asm__ __volatile__ ("sfence.vma %0"
				      : : "r" (start)
				      : "memory");
		start += PAGE_SIZE;
	}
}

#ifdef CONFIG_SMP

static void remote_sfence_vma(unsigned long start, unsigned long size)
{
	sbi_remote_sfence_vma(NULL, start, size);
}

static void remote_sfence_vma_asid(cpumask_t *mask, unsigned long start,
				   unsigned long size, unsigned long asid)
{
	cpumask_t hmask;

	cpumask_clear(&hmask);
	riscv_cpuid_to_hartid_mask(mask, &hmask);
	sbi_remote_sfence_vma_asid(hmask.bits, start, size, asid);
}


void flush_tlb_all(void)
{
	remote_sfence_vma(0, SFENCE_VMA_FLUSH_ALL);
}

void flush_tlb_mm(struct mm_struct *mm)
{
	remote_sfence_vma_asid(mm_cpumask(mm), 0, SFENCE_VMA_FLUSH_ALL, 0);
}

void flush_tlb_page(struct vm_area_struct *vma, unsigned long addr)
{
	remote_sfence_vma_asid(mm_cpumask(vma->vm_mm), addr, PAGE_SIZE, 0);
}


void flush_tlb_range(struct vm_area_struct *vma, unsigned long start,
		     unsigned long end)
{
	if (end - start > tlbi_range_threshold) {
		flush_tlb_mm(vma->vm_mm);
		return;
	}

	remote_sfence_vma_asid(mm_cpumask(vma->vm_mm), start, end - start, 0);
}

void flush_tlb_kernel_range(unsigned long start, unsigned long end)
{
	if (end - start > tlbi_range_threshold) {
		flush_tlb_all();
		return;
	}

	remote_sfence_vma(start, end - start);
}

#endif /* CONFIG_SMP */
