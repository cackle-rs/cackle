#include <stdio.h>

void __attribute__ ((constructor)) premain() {
    printf("nothing to see here\n");
}

void nothing_to_see_here() { }
