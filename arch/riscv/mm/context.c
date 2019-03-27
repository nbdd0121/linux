// SPDX-License-Identifier: GPL-2.0
/*
 * Copyright (C) 2012 Regents of the University of California
 * Copyright (C) 2017 SiFive
 * Copyright (C) 2019 Gary Guo, University of Cambridge
 * Copyright (C) 2019 Western Digital Corporation or its affiliates.
 */

#include <linux/bitops.h>
#include <linux/slab.h>
#include <linux/sched/signal.h>
#include <linux/mm.h>
#include <asm/tlbflush.h>
#include <asm/cacheflush.h>
#include <asm/mmu_context.h>

static unsigned asidlen;
static DEFINE_SPINLOCK(cpu_asid_lock);

#define NUM_ASIDS	(1UL << asidlen)
#define ASID_MASK	GENMASK(asidlen - 1, 0)
#define ASID_GENERATION	NUM_ASIDS

static atomic_long_t asid_generation;
static unsigned long *asid_map;

static DEFINE_PER_CPU(atomic_long_t, active_asids);
static DEFINE_PER_CPU(unsigned long, reserved_asids);

static bool check_reserved_asid(unsigned long asid, unsigned long newasid)
{
	unsigned int cpu;
	bool hit = false;

	/*
	 * Iterate over the set of reserved ASIDs looking for a match.
	 * If we find one, then we can update our mm to use newasid
	 * (i.e. the same ASID in the current generation) but we can't
	 * exit the loop early, since we need to ensure that all copies
	 * of the old ASID are updated to reflect the mm. Failure to do
	 * so could result in us missing the reserved ASID in a future
	 * generation.
	 */
	for_each_possible_cpu(cpu) {
		if (per_cpu(reserved_asids, cpu) == asid) {
			hit = true;
			per_cpu(reserved_asids, cpu) = newasid;
		}
	}

	return hit;
}

/* 64-bit would never overflow */
#if __riscv_xlen == 32
static void asid_generation_overflow(void) {
	unsigned long asid, newasid;
	struct task_struct *p;

	printk_deferred(KERN_INFO "ASID generation overflown\n");

	/*
	 * If a process is asleep for very long duration and wakes up only
	 * after ASID generation overflow, its ASID may alias with another MM
	 * context.
	 *
	 * If a CPU has always been running the same task and the task is never
	 * ran once on another hart, then the MM context's ASID may alias with
	 * another MM context.
	 *
	 * We tackle this issue by set ASID to 0 (i.e. never allocated) in the
	 * first case, and eagerly update ASID in the second case.  This is
	 * expensive operation, but is necessary for correctness and is super
	 * rare.
	 */
	write_lock(&tasklist_lock);

	for_each_process(p) {
		if (!p->mm)
			continue;

		asid = atomic_long_read(&p->mm->context.asid);
		if (!asid)
			continue;

		newasid = ASID_GENERATION | (asid & ASID_MASK);
		if (check_reserved_asid(asid, newasid)) {
			asid = newasid;
		} else {
			asid = 0;
		}

		atomic_long_set(&p->mm->context.asid, asid);
	}

	write_unlock(&tasklist_lock);
}
#endif

static void new_asid_generation(void)
{
	unsigned int cpu;
	unsigned long asid;
	unsigned long generation = atomic_long_read(&asid_generation);
	int overflow = check_add_overflow(generation, ASID_GENERATION,
					  &generation);

#if __riscv_xlen == 32
	if (unlikely(overflow))
		generation = ASID_GENERATION;
#else
	BUG_ON(overflow);
#endif

	/* No need to use atomic add, as this is only writer */
	atomic_long_set(&asid_generation, generation);

	/* Update the list of reserved ASIDs and the ASID bitmap. */
	bitmap_clear(asid_map, 0, NUM_ASIDS);

	for_each_possible_cpu(cpu) {
		asid = atomic_long_xchg_relaxed(&per_cpu(active_asids, cpu),
						0);

		/*
		 * If this CPU has already been through a
		 * rollover, but hasn't run another task in
		 * the meantime, we must preserve its reserved
		 * ASID, as this is the only trace we have of
		 * the process it is still running.
		 */
		if (asid == 0)
			asid = per_cpu(reserved_asids, cpu);
		__set_bit(asid & ASID_MASK, asid_map);
		per_cpu(reserved_asids, cpu) = asid;
	}

#if __riscv_xlen == 32
	/* Special handling is needed for generation overflow */
	if (unlikely(overflow))
		asid_generation_overflow();
#endif

	/* Flush TLB on all CPUs */
	flush_tlb_all();
}

static unsigned long alloc_asid(struct mm_struct *mm)
{
	static unsigned long cur_idx = 1;
	unsigned long asid = atomic_long_read(&mm->context.asid);
	unsigned long generation = atomic_long_read(&asid_generation);

	if (asid != 0) {
		unsigned long newasid = generation | (asid & ASID_MASK);

		/*
		 * If current ASID was active during a rollover, we can
		 * continue to use it. In such case the ASID appears in
		 * reserved_asids and the corresponding bit in asid_map is
		 * already set.
		 */
		if (check_reserved_asid(asid, newasid))
			return newasid;
	}

	/*
	 * Allocate a free ASID. If we can't find one, start a new generation.
	 * Note that ASID 0 is the special ASID used by software that does not
	 * have ASID support in mind, so avoid using it.
	 */
	asid = find_next_zero_bit(asid_map, NUM_ASIDS, cur_idx);
	if (likely(asid != NUM_ASIDS))
		goto set_asid;

	/* Running out of ASIDs. Start a new generation */
	new_asid_generation();
	generation = atomic_long_read(&asid_generation);

	/* We have more ASIDs than CPUs, so this will always succeed */
	asid = find_next_zero_bit(asid_map, NUM_ASIDS, 1);
	BUG_ON(asid == NUM_ASIDS);

set_asid:
	__set_bit(asid, asid_map);
	cur_idx = asid;
	return asid | generation;
}

/*
 * When necessary, performs a deferred icache flush for the given MM context,
 * on the local CPU.  RISC-V has no direct mechanism for instruction cache
 * shoot downs, so instead we send an IPI that informs the remote harts they
 * need to flush their local instruction caches.  To avoid pathologically slow
 * behavior in a common case (a bunch of single-hart processes on a many-hart
 * machine, ie 'make -j') we avoid the IPIs for harts that are not currently
 * executing a MM context and instead schedule a deferred local instruction
 * cache flush to be performed before execution resumes on each hart.  This
 * actually performs that local instruction cache flush, which implicitly only
 * refers to the current hart.
 */
static inline void flush_icache_deferred(struct mm_struct *mm)
{
#ifdef CONFIG_SMP
	unsigned int cpu = smp_processor_id();
	cpumask_t *mask = &mm->context.icache_stale_mask;

	if (cpumask_test_cpu(cpu, mask)) {
		cpumask_clear_cpu(cpu, mask);
		/*
		 * Ensure the remote hart's writes are visible to this hart.
		 * This pairs with a barrier in flush_icache_mm.
		 */
		smp_mb();
		local_flush_icache_all();
	}

#endif
}

static void switch_mm_asid(struct mm_struct *next, unsigned int cpu)
{
	unsigned long flags, asid, old_active_asid;

	/*
	 * - If old_active_asid is 0, it means we just encountered a rollover.
	 *   In which case we might need to have our TLB flushed.
	 *
	 * - If the ASID is not in the current generation, it means we need to
	 *   allocate new ASID for this mm_struct.
	 *
	 * - If the cmpxchg failed it means that there is a rollover that is
	 *   only visible to us after reading asid_generation. In which case we
	 *   also need to fall to slow path.
	 */
	asid = atomic_long_read(&next->context.asid);
	old_active_asid = atomic_long_read(&per_cpu(active_asids, cpu));
	if (old_active_asid &&
	    (asid &~ ASID_MASK) == atomic_long_read(&asid_generation) &&
	    atomic_long_cmpxchg_relaxed(&per_cpu(active_asids, cpu),
					old_active_asid, asid))
		goto switch_mm_fast;

	spin_lock_irqsave(&cpu_asid_lock, flags);

	/* If ASID is from old generation, re-allocate */
	asid = atomic_long_read(&next->context.asid);
	if ((asid &~ ASID_MASK) != atomic_long_read(&asid_generation)) {
		asid = alloc_asid(next);
		/*
		 * After a rollover old harts no longer have cached
		 * contents of this MM context except for those
		 * currently running.
		 */
		cpumask_copy(&next->context.cache_mask, mm_cpumask(next));
		atomic_long_set(&next->context.asid, asid);
	}

	atomic_long_set(&per_cpu(active_asids, cpu), asid);
	spin_unlock_irqrestore(&cpu_asid_lock, flags);

switch_mm_fast:
	/*
	 * Mark this hart as potentially having cached TLB of this MM context
	 */
	cpumask_set_cpu(cpu, &next->context.cache_mask);

	csr_write(CSR_SATP, virt_to_pfn(next->pgd) | SATP_MODE |
			 (asid & ASIDMAX_MASK) << SATP_ASID_SHIFT);
}

static void switch_mm_noasid(struct mm_struct *prev, struct mm_struct *next,
			     unsigned int cpu)
{
	/*
	 * When ASID is not used, only harts actively running code can possibly
	 * have translation entries cached.
	 */
	cpumask_clear_cpu(cpu, &prev->context.cache_mask);
	cpumask_set_cpu(cpu, &next->context.cache_mask);

	csr_write(CSR_SATP, virt_to_pfn(next->pgd) | SATP_MODE);
	local_flush_tlb_mm(next);
}

void switch_mm(struct mm_struct *prev, struct mm_struct *next,
	struct task_struct *task)
{
	unsigned int cpu;

	if (unlikely(prev == next))
		return;

	/*
	 * Mark the current MM context as inactive, and the next as
	 * active.  This is at least used by the icache flushing
	 * routines in order to determine who should be flushed.
	 */
	cpu = smp_processor_id();

	cpumask_clear_cpu(cpu, mm_cpumask(prev));
	cpumask_set_cpu(cpu, mm_cpumask(next));

#ifdef CONFIG_MMU
	if (asidlen != 0)
		switch_mm_asid(next, cpu);
	else
		switch_mm_noasid(prev, next, cpu);
#endif

	flush_icache_deferred(next);
}

/*
 * Get ASIDLEN supported by the current CPU. This function relies on the fact
 * that head.S sets all possible bits in SATP_ASID to 1, so it must be called
 * after hart boot and before any context switch happens.
 */
static unsigned get_cpu_asidlen(void)
{
	unsigned long asid_set = csr_read(sptbr) & SATP_ASID;
	/*
	 * Privileged ISA 1.10 spec says that implemented bits will hold 1, and
	 * least significant bits are implemented first.
	 */
	unsigned asidlen = fls_long(asid_set >> SATP_ASID_SHIFT);
	return asidlen;
}

/* Check if the current cpu's ASIDLEN is compatible with asidlen */
void verify_cpu_asidlen(void)
{
	unsigned asid;

	if (!asidlen) {
		csr_write(sptbr, csr_read(sptbr) &~ SATP_ASID);
		return;
	}

	asid = get_cpu_asidlen();
	if (asid != asidlen) {
		/* We assume all cores to have the same ASIDLEN */
		panic("CPU%d's ASIDLEN(%u) different from boot CPU's (%u)\n",
			smp_processor_id(), asid, asidlen);
	}
}

static int asids_init(void)
{
	unsigned int cpu;

	asidlen = get_cpu_asidlen();
	if (!asidlen) {
		pr_info("ASID is not supported\n");
		return 0;
	}

	pr_info("ASIDLEN = %u\n", asidlen);

	/*
	 * Even though the spec currently suggests ASID space to be
	 * hart-local, it is still easier to manage it as a global resource to
	 * reduce cost of cross-hart TLB invalidation.
	 *
	 * If we have more CPUs than number of ASIDs, just don't use it.
	 */
	if (NUM_ASIDS - 1 <= num_possible_cpus()) {
		pr_warn("Not enough ASIDs(%lu) for number of CPUs(%u). ASID is disabled\n",
			NUM_ASIDS, num_possible_cpus());
		asidlen = 0;

		/*
		 * Disable ASID support, revert to use ASID 0 instead.  No need
		 * to flush TLB now, as switch_mm will flush it.
		 */
		csr_write(sptbr, csr_read(sptbr) &~ SATP_ASID);

		return 0;
	}

	atomic_long_set(&asid_generation, ASID_GENERATION);
	asid_map = kcalloc(BITS_TO_LONGS(NUM_ASIDS), sizeof(*asid_map),
			   GFP_KERNEL);
	if (!asid_map)
		panic("Failed to allocate bitmap for %lu ASIDs\n", NUM_ASIDS);

	/*
	 * When starting up all possible bits in SATP_ASID are set, which
	 * corresponds to the last ASID. So do not use in the first generation.
	 */
	for_each_possible_cpu(cpu)
		atomic_long_set(&per_cpu(active_asids, cpu), ASID_MASK);
	__set_bit(ASID_MASK, asid_map);

	pr_info("ASID allocator initialised with %lu entries\n", NUM_ASIDS);

	return 0;
}
early_initcall(asids_init);
