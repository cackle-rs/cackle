#!/bin/python3

# Copyright 2023 The Cackle Authors
# 
# Licensed under the Apache License, Version 2.0 <LICENSE or
# https://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE or
# https://opensource.org/licenses/MIT>, at your option. This file may not be
# copied, modified, or distributed except according to those terms.


import seccomp
f = seccomp.SyscallFilter(seccomp.ALLOW)
#f.add_rule(seccomp.TRAP, "mknodat")

# Disallow TIOCSTI
f.add_rule(seccomp.ERRNO(1), "ioctl", seccomp.Arg(1, seccomp.EQ, 0x5412))

#f = seccomp.SyscallFilter(seccomp.ERRNO(1))
if False:
    f = seccomp.SyscallFilter(seccomp.LOG)
    f.add_rule(seccomp.ALLOW, "access")
    f.add_rule(seccomp.ALLOW, "openat")
    f.add_rule(seccomp.ALLOW, "newfstatat")
    f.add_rule(seccomp.ALLOW, "mmap")
    f.add_rule(seccomp.ALLOW, "close")
    f.add_rule(seccomp.ALLOW, "read")
    f.add_rule(seccomp.ALLOW, "pread64")
    f.add_rule(seccomp.ALLOW, "mprotect")
    f.add_rule(seccomp.ALLOW, "munmap")
    f.add_rule(seccomp.ALLOW, "brk")
    f.add_rule(seccomp.ALLOW, "dup2")
    f.add_rule(seccomp.ALLOW, "utimensat")
    f.add_rule(seccomp.ALLOW, "exit_group")
    f.add_rule(seccomp.ALLOW, "arch_prctl")
    f.add_rule(seccomp.ALLOW, "execve")

file = open("a.bpf", "w")
f.export_bpf(file)
file.close()
