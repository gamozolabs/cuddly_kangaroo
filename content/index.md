```templateinfo
title = "Home | Gamozo Labs"
description = "The homepage of Gamozo Labs!"
style = "post.css"
template = "post.html"
time = "2016-11-08T00:00:00+00:00"
```

# :smile: :poop: :crab:

```asm
[bits 64]

%define FLUT_BIN_SIZE 2

; rsi <- flut vector
flut_alloc:
	push rax
	push rcx
	push rdi

	; Check if we're out of flut entries
	cmp qword [gs:thread_local.flut_pages_rem], 0
	jne short .dont_alloc_page

	; Allocate a whole page
	mov rsi, 4096
	rand_alloc rsi

	; Zero out the page
	mov rdi, rsi
	mov ecx, (4096 / 8)
	xor eax, eax
	rep stosq

	; Save the new flut pages
	mov qword [gs:thread_local.flut_pages],     rsi
	mov qword [gs:thread_local.flut_pages_rem], 4096 / ((1 << FLUT_BIN_SIZE)*8)

.dont_alloc_page:
	; Consume one flut allocation
	mov  rsi, ((1 << FLUT_BIN_SIZE) * 8)
	xadd qword [gs:thread_local.flut_pages], rsi
	dec  qword [gs:thread_local.flut_pages_rem]

	pop rdi
	pop rcx
	pop rax
	ret

; rcx  -> flut
; xmm5 -> hash
; rcx  <- flut entry or pointer to flut entry to fill
; CF   <- Set if flut needs to be filled
flut_fetch_or_lock:
	push rax
	push rbx
	push rdx
	push rsi
	push rbp

	movq rdx, xmm5

	mov rbx, (128 / FLUT_BIN_SIZE)
.for_each_bin:
	mov ebp, edx
	and ebp, ((1 << FLUT_BIN_SIZE) - 1)
	ror rdx, FLUT_BIN_SIZE

	; Check if this entry has been filled
	mov rsi, qword [rcx + rbp*8]
	cmp rsi, 2
	jae short .filled

	; The entry has not been filled, atomicially try to lock it down so we
	; can fill it!
	xor  eax, eax
	mov  esi, 1
	lock cmpxchg qword [rcx + rbp*8], rsi
	jne  short .wait_for_fill

	; We succeeded in obtaining the lock. If this is the last bin in the hash
	; we want to return to the user to fill in this last piece.
	lea rax, [rcx + rbp*8]
	cmp rbx, 1
	je  short .done_needs_fill

	; We obtained the lock and we are not at the last level, allocate another
	; array and place it in the flut.
	call flut_alloc
	
	mov qword [rcx + rbp*8], rsi
	jmp short .filled

.wait_for_fill:
	; We lost the race in locking, wait for the caller with the lock to fill
	; it in and then treat it as a filled entry.
	pause
	cmp qword [rcx + rbp*8], 1
	jbe short .wait_for_fill
	mov rsi, qword [rcx + rbp*8]

.filled:
	cmp rbx, (64 / FLUT_BIN_SIZE) + 1
	jne short .dont_grab_upper

	pextrq rdx, xmm5, 1

.dont_grab_upper:
	mov rcx, rsi
	dec rbx
	jnz short .for_each_bin

	; At this point the entry of the flut was filled in, return the value
	; in the flut and clear CF.
	mov rax, rcx

	clc
	jmp short .done
.done_needs_fill:
	stc
.done:
	mov rcx, rax
	pop rbp
	pop rsi
	pop rdx
	pop rbx
	pop rax
	ret

; rcx -> flut
; rax <- random entry from flut
global flut_random
flut_random:
	push rbx
	push rcx
	push rdx
	push rbp
	push rdi

	XMMPUSH xmm15

	call falkrand
	movq rbp, xmm15

	xor ebx, ebx
.lewp:
	mov eax, ebp
	and eax, ((1 << FLUT_BIN_SIZE) - 1)
	ror rbp, FLUT_BIN_SIZE

	mov edi, (1 << FLUT_BIN_SIZE)
.try_find_next:
	cmp qword [rcx + rax*8], 2
	jae short .found

	inc eax
	and eax, ((1 << FLUT_BIN_SIZE) - 1)
	dec edi
	jnz short .try_find_next
	jmp short .fail

.found:
	mov rcx, [rcx + rax*8]
	inc ebx
	cmp ebx, (64 / FLUT_BIN_SIZE)
	jne short .dont_get_high

	pextrq rbp, xmm15, 1

.dont_get_high:
	cmp ebx, (128 / FLUT_BIN_SIZE)
	jb  short .lewp

	mov rax, rcx
	jmp short .done
.fail:
	xor eax, eax
.done:
	XMMPOP xmm15

	pop rdi
	pop rbp
	pop rdx
	pop rcx
	pop rbx
	ret 

struc fht_table
	.entries: resq 1 ; Number of used entries in this table
	.bits:    resq 1 ; Number of bits in this table
	.table:   resq 1 ; Pointer to hash table
	.ents:    resq 1 ; Pointer to list of hashes
endstruc

struc fht_entry
	.hash: resq 2
	.data: resq 1
	.pad:  resq 1
endstruc

struc fht_list_entry
	.hash: resq 2
endstruc

; rcx -> Number of bits
; rcx <- Hash table base
fht_create:
	push rax
	push rdi
	push rbp

	; Allocate the table header
	mov rbp, fht_table_size
	rand_alloc rbp

	; Initialize the table header
	mov qword [rbp + fht_table.entries], 0
	mov qword [rbp + fht_table.bits],    rcx

	; Calculate the table size
	mov eax, 1
	shl rax, cl

	; Allocate and zerothe hash table
	imul rdi, rax, fht_entry_size
	mov  rcx, rdi
	mixed_alloc rdi
	call bzero
	mov  [rbp + fht_table.table], rdi

	; Allocate and zero the hash list table
	imul rdi, rax, fht_list_entry_size
	mov  rcx, rdi
	mixed_alloc rdi
	call bzero
	mov  [rbp + fht_table.ents], rdi

	mov rcx, rbp

	pop rbp
	pop rdi
	pop rax
	ret

; rcx -> Pointer to hash table
; rax <- Random entry (or zero if no entries are present)
fht_random:
	push rbx
	push rcx
	push rdx
	push r15

	XMMPUSH xmm5

	cmp qword [rcx + fht_table.entries], 0
	je  short .fail

	; Pick a random entry
	call xorshift64
	xor  rdx, rdx
	mov  rax, r15
	div  qword [rcx + fht_table.entries]

	; Calculate the entry offset
	mov  rbx, [rcx + fht_table.ents]
	imul rdx, fht_list_entry_size

	; Fetch the random hash. If it is zero, fail.
	movdqu xmm5, [rbx + rdx + fht_list_entry.hash]
	ptest  xmm5, xmm5
	jz     short .fail

	; Look up the hash. This will always succeed.
	call fht_fetch_or_lock
	mov  rax, rcx
	jmp  short .done

.fail:
	xor rax, rax
.done:
	XMMPOP xmm5

	pop r15
	pop rdx
	pop rcx
	pop rbx
	ret

; rcx  -> Pointer to hash table
; xmm5 -> Hash
; rcx  <- Pointer to entry or entry (depending on CF)
; CF   <- Set if this is a new entry we must populate
fht_fetch_or_lock:
	push rax
	push rbx
	push rdx
	push rdi
	push rbp

	XMMPUSH xmm4

	; Save off the hash table pointer
	mov rbp, rcx

	mov rbx, [rbp + fht_table.table]
	mov rcx, [rbp + fht_table.bits]

	; rbx now points to the start of the hash table vector
	; rcx is now the number of bits in the hash table

	; Get the low 64-bits of the hash
	movq rdx, xmm5

	; Calculate the mask
	mov eax, 1
	shl rax, cl
	dec rax

	; Mask the hash
	and rdx, rax

	; Calculate the byte offset into the hash table
	imul rdx, fht_entry_size

.next_entry:
	; Look up this entry in the table
	movdqu xmm4, [rbx + rdx + fht_entry.hash]

	; If this bin is empty, try to fill it
	ptest xmm4, xmm4
	jz    short .empty

	; If the hashes match, we have an entry!
	pxor  xmm4, xmm5
	ptest xmm4, xmm4
	jz    short .found

	; The bin was not empty, nor did our hash match. This is a collision case.
	; Go to the next entry (linear probing)
	add rdx, fht_entry_size
	and rdx, rax
	jmp short .next_entry

.empty:
	; The bin was empty, try to win the race to fill it.
	lea rdi, [rbx + rdx + fht_entry.hash]

	; We did not find an entry, try to atomicially populate this entry
	push rax
	push rbx
	push rcx
	push rdx

	; Compare part
	xor edx, edx
	xor eax, eax

	; Exchange part
	pextrq rcx, xmm5, 1
	pextrq rbx, xmm5, 0

	lock cmpxchg16b [rdi]

	; If we lost, rdx:rax is the 128-bit value that we lost to. Store this in
	; xmm4.
	pinsrq xmm4, rdx, 1
	pinsrq xmm4, rax, 0

	pop  rdx
	pop  rcx
	pop  rbx
	pop  rax
	je   short .won_race

	; We lost the race. Check if the hash matches (we could lose the race to
	; a collision case).
	pxor  xmm4, xmm5
	ptest xmm4, xmm4
	jz    short .found

	; We lost the race, and it was a collision. Go to the next entry.
	add rdx, fht_entry_size
	and rdx, rax
	jmp short .next_entry

.won_race:
	; We won the race! Return the address of the data to fill.
	lea rcx, [rbx + rdx + fht_entry.data]

	; Get this entry's ID
	mov  edi, 1
	lock xadd qword [rbp + fht_table.entries], rdi

	; Add this hash to the hash list
	mov    rbx, [rbp + fht_table.ents]
	imul   rdi, fht_list_entry_size
	movdqu [rbx + rdi + fht_list_entry.hash], xmm5

	XMMPOP xmm4

	pop rbp
	pop rdi
	pop rdx
	pop rbx
	pop rax
	stc
	ret

.found:
	; Fetch the data. If it is zero, loop until it is not.
	mov  rcx, [rbx + rdx + fht_entry.data]
	test rcx, rcx
	jz   short .found

	XMMPOP xmm4

	pop rbp
	pop rdi
	pop rdx
	pop rbx
	pop rax
	clc
	ret
```

# Intro

In this blog I'm going to introduce you to a concept I've been working on for almost 2 years now. _Vectorized emulation_. The goal is to take standard applications and JIT them to their AVX-512 equivalent such that we can fuzz 16 VMs at a time per thread. The net result of this work allows for high performance fuzzing (approx 40 billion to 120 billion instructions per second [the 2 trillion clickbait number is theoretical maximum]) depending on the target, while gathering differential coverage on code, register, and memory state.

By gathering more than just code coverage we are able to track state of code deeper than just code coverage itself, allowing us to fuzz through things like `memcmp()` without any hooks or static analysis of the target at all.

Further since we're running emulated code we are able to run a soft MMU implementation which has byte-level permissions. This gives us stronger-than-ASAN memory protections, making bugs fail faster and cleaner.

# How it came to be an idea

My history with fuzzing tools starts off effectively with my hypervisor for fuzzing, [falkervisor][falkervisor]. falkervisor served me well for quite a long time, but my work rotated more towards non-x86 targets, which it did not support. With a demand for emulation I made modifications to QEMU for high-performance fuzzing, and ultimately swapped out their MMU implementation for my own which has byte-level permissions. This new byte-level permission model allowed me to catch even the smallest memory corruptions, leading to finding pretty fun bugs!

More and more after working with QEMU I got annoyed. It's designed for whole systems yet I was using it for fuzzing targets that were running with unknown hardware and running from dynamically dumped memory snapshots. Due to the level of abstraction in QEMU I started to get concerned with the potential unknowns that would affect the instrumentation and fuzzing of targets.

I developed my first MIPS emulator. It was not designed for performance, but rather purely for simple usage and perfect single stepping. You step an instruction, registers and memory get updated. No JIT, no intermediate registers, no flushing or weird block level translation changes. I eventually made a JIT for this that maintained the flush-state-every-instruction model and successfully used it against multiple targets. I also developed an ARM emulator somewhere in this timeframe.

When early 2017 rolls around I'm bored and want to buy a Xeon Phi. Who doesn't want a 64-core 256-thread single processor? I really had no need for the machine so I just made up some excuse in my head that the high bandwidth memory on die would make reverting snapshots faster. Yeah... like that really matters? Oh well, I bought it.

While the machine was on the way I had this idea... when fuzzing from a snapshot all VMs initially start off fuzzing with the exact same state, except for maybe an input buffer and length being changed. Thus they do identical operations until user-controlled data is processed. I've done some fun vectorization work before, but what got me thinking is why not just emit `vpaddd` instead of `add` when JITting, and now I can run 16 VMs at a time!

Alas... the idea was born

# A primer on snapshot fuzzing

Snapshot fuzzing is fundamental to this work and almost all fuzzing work I have done from 2014 and beyond. It warrants its own blog entirely.

Snapshot fuzzing is a method of fuzzing where you start from a partially-executed system state. For example I can run an application under GDB, like a parser, put a breakpoint after the file/network data has been read, and then dump memory and register state to a core dump using `gcore`. At this point I have full memory and register state for the application. I can then load up this core dump into any emulator, set up memory contents and permissions, set up register state, and continue execution. While this is an example with core dumps on Linux, this methodology works the same whether the snapshot is a core dump from GDB, a minidump on Windows, or even an exotic memory dump taken from an exploit on a locked-down device like a phone.

All that matters is that I have memory state and register state. From this point I can inject/modify the file contents in memory and continue execution with a new input!

It can get a lot more complex when dealing with kernel state, like file handles, network packets buffered in the kernel, and really anything that syscalls. However in most targets you can make some custom rigging using `strace` to know which FDs line up, where they are currently seeked, etc. Further a full system snapshot can be used instead of a single application and then this kernel state is no longer a concern.

The benefits of snapshot fuzzing are performance (linear scaling), high levels of introspection (even without source or symbols), and most importantly... determinism. Unless the emulator has bugs snapshot fuzzing is typically deterministic (sometimes relaxed for performance). Find some super exotic race condition while snapshot fuzzing? Well, you can single step through with the same input and now you can look at the trace as a human, even if it's a 1 in a billion chance of hitting.

# A primer on vectorized instruction sets

Since the 90s many computer architectures have some form of SIMD (vectorized) instruction set. SIMD stands for single instruction multiple data. This means that a single instruction performs an operation (typically the same) on multiple different pieces of data. SIMD instruction sets fall under names like MMX, SSE, AVX, AVX512 for x86, NEON for ARM, and AltiVec for PPC. You've probably seen these instructions if you've ever looked at a `memcpy()` implementation on any 64-bit x86 system. They're the ones with the gross 15 character mnemonics and registers you didn't even know existed.

For a simple case lets talk about standard SSE on x86. Since x86_64 started with the Pentium 4 and the Pentium 4 had up to SSE3 implementations, almost any x86_64 compiler will generate SSE instructions as they're always valid on 64-bit systems.

SSE provides 128-bit SIMD operations to x86. SSE introduced 16 128-bit registers named `xmm0` through `xmm15` (only 8 `xmm` registers on 32-bit x86). These 128-bit registers can be treated as groups of different sized smaller pieces of data which sum up to 128 bits.

- 4 single precision floats
- 2 double precision floats
- 2 64-bit integers
- 4 32-bit integers
- 8 16-bit integers
- 16 8-bit integers

Now with a single instruction it is possible to perform the same operation on multiple floats or integers. For example there is an instruction `paddd`, which stands for packed add dwords. This means that the 128-bit registers provided are treated as 4 32-bit integers, and an add operation is performed.

Here's a real example, adding `xmm0` and `xmm1` together treating them as 4 individual 32-bit integer lanes and storing them back into `xmm0`

`paddd xmm0, xmm1`

## Horizontal Rules

___

# Rust is good

```rs
/// Initializes the local APIC for the current core
///
/// # Returns
///
/// The initialized APIC structure
///
pub unsafe fn init() -> Self {
    /// The x2apic enable bit in the `IA32_APIC_BASE` MSR
    const IA32_APIC_BASE_EXTD: u64 = 1 << 10;

    /// The global enable bit in the `IA32_APIC_BASE` MSR
    const IA32_APIC_BASE_EN: u64 = 1 << 11;

    /// MSR for the `IA32_APIC_BASE`
    const IA32_APIC_BASE: u32 = 0x1b;

    // Get the CPU features
    let features = arch::x86_64::get_cpu_features();

    // Make sure the the APIC is supported
    assert!(features.apic, "APIC must be supported");

    // Load the APIC base register
    let mut apic_base_msr = arch::x86_64::rdmsr(IA32_APIC_BASE);

    // We cannot always re-enable the APIC, thus make sure it was not
    // disabled by the firmware.
    assert!((apic_base_msr & IA32_APIC_BASE_EN) != 0,
        "APIC was disabled during firmware execution");

    // Get the base address where the APIC is mapped
    let apic_base = apic_base_msr & 0xf_ffff_f000;

    // Enable the x2apic if supported
    if features.x2apic {
        apic_base_msr |= IA32_APIC_BASE_EXTD;
    }

    // Reprogram the APIC as some settings may have changed
    arch::x86_64::wrmsr(IA32_APIC_BASE, apic_base_msr);

    Apic {
        mode: if features.x2apic {
            ApicMode::X2Apic
        } else {
            ApicMode::XApic(
                core::slice::from_raw_parts_mut(apic_base as *mut u32,
                                                4096 / size_of::<u32>())
            )
        },
    }
}

/// Get the APIC ID of the current running core
///
/// # Returns
///
/// The APIC ID for the current processor
///
pub fn apic_id(&self) -> u32 {
    // Read the APIC ID register
    let apic_id = unsafe { self.read(Register::ApicId) };

    // Adjust the APIC ID based on the current APIC mode
    match &self.mode {
        ApicMode::XApic(_) => apic_id >> 24,
        ApicMode::X2Apic   => apic_id,
    }
}
```
