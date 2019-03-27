// SPDX-License-Identifier: GPL-2.0

#include <linux/mm.h>
#include <linux/smp.h>
#include <linux/sched.h>
#include <asm/sbi.h>

void flush_tlb_all(void)
{
	sbi_remote_sfence_vma(NULL, 0, -1);
}

static void __sbi_tlb_flush_range(struct mm_struct *mm, unsigned long start,
				  unsigned long size)
{
    struct cpumask *cmask = mm_cpumask(mm);
	struct cpumask hmask;
	unsigned int cpuid;
	unsigned long asid = ASID(mm);

	if (cpumask_empty(cmask))
		return;

	cpuid = get_cpu();

	riscv_cpuid_to_hartid_mask(cmask, &hmask);
	sbi_remote_sfence_vma_asid(cpumask_bits(&hmask), start, size, asid);

	put_cpu();
}

void flush_tlb_mm(struct mm_struct *mm)
{
	__sbi_tlb_flush_range(mm, 0, -1);
}

void flush_tlb_page(struct vm_area_struct *vma, unsigned long addr)
{
	__sbi_tlb_flush_range(vma->vm_mm, addr, PAGE_SIZE);
}

void flush_tlb_range(struct vm_area_struct *vma, unsigned long start,
		     unsigned long end)
{
	__sbi_tlb_flush_range(vma->vm_mm, start, end - start);
}

void flush_tlb_kernel_range(unsigned long start, unsigned long end)
{
	sbi_remote_sfence_vma(NULL, start, end);
}
