// Triggers the `list_empty(head)` flag-cmp shape on x86_64.
//
// `list_empty` in the Linux kernel is `head->next == head`.  GCC at
// `-O2` lifts this to `cmp QWORD PTR [rdi+K], rdi+K` (mem vs reg+K),
// which Sleigh expands into a flag-tree the lifter normalises to:
//
//     Equal(Add(LOAD(rdi+K), Neg(Add(rdi, K))), 0)
//
// `FlagCmpCanonicalize` must fold this back into the canonical
//
//     Equal(LOAD(rdi+K), Add(rdi, K))
//
// shape so the pattern
//
//     int_eq(load(addr=add(<base>, K)), add(<base>, K))
//
// fires.  When the test compiles this file the resulting binary has
// `is_thread_group_empty` doing exactly that compare.

struct list_head {
    struct list_head *next;
    struct list_head *prev;
};

struct task {
    int pid;
    char pad[60];
    struct list_head head;
};

int is_thread_group_empty(struct task *t) {
    return t->head.next == &t->head;
}

int main(int argc, char **argv) {
    (void)argv;
    static struct task t;
    t.head.next = &t.head;
    t.head.prev = &t.head;
    return is_thread_group_empty(&t) + argc;
}
