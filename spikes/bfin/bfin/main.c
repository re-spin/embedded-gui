/* Simulator bench for the embedded-gui spike (bfin-elf-run --environment virtual).
 * stdout: every Frame/Dirty mark's framebuffer (raw X1RGB555), then u32 words:
 * status, stack bytes used, mark count, (kind, cycles, soft-float calls) per
 * mark, then total calls per wrapped soft-float helper. */
#include <stdint.h>
extern uint32_t spike_run(uint16_t *fb, void (*mark)(uint32_t, const uint16_t *));
extern uint32_t read_cycles(void);
extern int sys_write(int, const void *, unsigned);
extern char stack_top[]; /* linker _stack_top */
extern uint32_t sf_calls[]; /* softfloat_count.S: calls per wrapped helper */
extern const uint32_t sf_n;

#define STACK_PAINT 0xF000u
static uint16_t fb[480 * 234];
static uint32_t out[3 + 3 * 32 + 64];
static unsigned n;
static uint32_t last, last_sf;

static uint32_t sf_total(void)
{
    uint32_t t = 0;
    for (uint32_t i = 0; i < sf_n; i++)
        t += sf_calls[i];
    return t;
}

static void mark(uint32_t kind, const uint16_t *f)
{
    uint32_t now = read_cycles();
    uint32_t sf = sf_total();
    out[3 + 3 * n] = kind;
    out[4 + 3 * n] = now - last;
    out[5 + 3 * n] = sf - last_sf;
    last_sf = sf;
    n++;
    if (kind == 1 || kind == 2)
        sys_write(1, f, sizeof(fb));
    last = read_cycles();
}

/* Rust panic handler: print file:line (hex) to stderr and exit(3). */
void spike_panic(const char *file, unsigned len, uint32_t line)
{
    char num[10] = "0x0000\n"; /* line in hex: no libgcc division here */
    for (int i = 0; i < 4; i++)
        num[5 - i] = "0123456789abcdef"[(line >> (4 * i)) & 15];
    sys_write(2, "panic at ", 9);
    sys_write(2, file, len);
    sys_write(2, ":", 1);
    sys_write(2, num, 7);
    __asm__ volatile("R0 = 3; [--SP] = R0; R0 = SP; P0 = 1; EXCPT 0;");
    for (;;)
        ;
}

int main(void)
{
    volatile uint8_t *low = (volatile uint8_t *)(stack_top - STACK_PAINT);
    uint8_t probe;
    uintptr_t sp = (uintptr_t)&probe;
    for (volatile uint8_t *p = low; (uintptr_t)p < sp - 64; p++)
        *p = 0xA5;
    last = read_cycles();
    out[0] = spike_run(fb, mark);
    volatile uint8_t *p = low;
    while (*p == 0xA5)
        p++;
    out[1] = (uint32_t)(sp - (uintptr_t)p);
    out[2] = n;
    for (uint32_t i = 0; i < sf_n; i++)
        out[3 + 3 * n + i] = sf_calls[i];
    sys_write(1, out, (3 + 3 * n + sf_n) * 4);
    return 0;
}
