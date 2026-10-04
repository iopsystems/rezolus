#ifndef BTF_READ_H
#define BTF_READ_H

#include <bpf/bpf_core_read.h>

/*
 * BTF_READ(btf, ptr, field) - read one field of a kernel struct
 * @btf: a compile-time constant; true when @ptr is a BTF-typed pointer (a
 *       tp_btf, fentry or fexit argument, or bpf_get_current_task_btf())
 *
 * With @btf it is a plain load, which the verifier emits inline as a guarded
 * load. Otherwise it is BPF_CORE_READ, one bpf_probe_read_kernel() call. A
 * sampler with tp_btf/raw_tp or fentry/kprobe twins passes true from the BTF
 * program and false from the other, and the compiler removes the branch it
 * does not take.
 */
#define BTF_READ(btf, ptr, field) ((btf) ? (ptr)->field : BPF_CORE_READ(ptr, field))

#endif // BTF_READ_H
