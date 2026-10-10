/* _spike_fill_rows(const struct FillArgs *a): a solid block of h rows,
   C ABI.  The Rust wrapper (spikes/bfin/src/lib.rs, firmware::fill_rows)
   bounds-checks the block, needs an even stride, and hands over:

     0  u16 *first    top-left pixel
     4  int  dstep    bytes between rows, a multiple of 4
     8  u32  words    32-bit stores a row; 0 for w < 4
    12  u32  h        rows, >= 1
    16  u32  v        the pixel in both halves
    20  u32 *start    first 4-aligned pixel pair of the top row; for w < 4
                      the middle pixel (first + w / 2)
    24  u16 *last     top-right pixel
    28  int  next     dstep - 4 * words: from a row's word end to the next start

   Rows have the same alignment, so each row is the same: its first and
   last pixel as halfwords (inside the word span when they are aligned,
   rewritten with the same value), then the words between.  Below 4 px
   the first, middle and last pixel are the whole row.  Writes R0-R1,
   P0-P2, LC0/1, LT0/1, LB0/1; saves P3-P5.  */
	.section .text.spike_fill_rows,"ax",@progbits
	.align 4
	.global _spike_fill_rows
	.type _spike_fill_rows, STT_FUNC
_spike_fill_rows:
	[--SP] = (P5:3);
	P2 = R0;
	P3 = [P2];
	P4 = [P2 + 4];
	P1 = [P2 + 8];
	R1 = [P2 + 12];
	R0 = [P2 + 16];
	P0 = [P2 + 20];
	P5 = [P2 + 24];
	P2 = [P2 + 28];
	LC1 = R1;
	CC = P1 == 0;
	IF CC JUMP .Lnarrow;
	LSETUP (.Lrow, .Lrow_end) LC1;
.Lrow:
	W[P3] = R0;
	P3 = P3 + P4;
	LSETUP (.Lw, .Lw) LC0 = P1;
.Lw:
	[P0++] = R0;
	W[P5] = R0;
	P0 = P0 + P2;
.Lrow_end:
	P5 = P5 + P4;
	(P5:3) = [SP++];
	RTS;
.Lnarrow:
	LSETUP (.Ln, .Ln_end) LC1;
.Ln:
	W[P3] = R0;
	W[P0] = R0;
	W[P5] = R0;
	P3 = P3 + P4;
	P0 = P0 + P4;
.Ln_end:
	P5 = P5 + P4;
	(P5:3) = [SP++];
	RTS;
	.size _spike_fill_rows, .-_spike_fill_rows
