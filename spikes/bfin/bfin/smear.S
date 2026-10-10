/* _spike_smear_line(const struct SmearArgs *a): the inner loops of
   embedded-gui's box smear (Rgb555Surface::smear_x/smear_y) for one line of
   X1RGB555 pixels, C ABI.  The Rust wrapper (spikes/bfin/src/lib.rs,
   firmware::smear_line) bounds-checks the line, fills the ring and the sum,
   and hands over:

     0  u16 *out      first pixel of the line (output k goes here, k = 0..)
     4  int  step     bytes between pixels (2 for a row, 2 * width for a column)
     8  u32 *ring     taps entries, 4-aligned: spread(pixel k - half) at slot k
    12  u32  ring_len taps * 4 (bytes)
    16  int  -shift   log2(taps), negated (LSHIFT by a negative count is >>)
    20  u32  sum      sum of the ring
    24  u32  main     pixels whose lead (k + half) is inside the line
    28  u32  tail     the rest (half), whose lead is the repeated end pixel
    32  u32  end      spread(last pixel)
    36  u16 *lead     out + half * step

   spread(p) = (p | p << 16) & 0x03e07c1f puts B, R and G 5 bits apart, so
   one add sums all three.  The ring is a circular buffer (I0/B0/L0): the
   value leaving the window and the one entering share a slot, so each pixel
   is one ring load and one store.  Writes R0-R3, P0-P2, I0, B0, L0 (restored
   to 0 as the ABI requires), LC0, LT0, LB0, ASTAT; saves R4-R7.  */
	.section .text.spike_smear_line,"ax",@progbits
	.align 4
	.global _spike_smear_line
	.type _spike_smear_line, STT_FUNC
_spike_smear_line:
	[--SP] = (R7:4);
	P2 = R0;
	P0 = [P2];
	R0 = [P2 + 8];
	I0 = R0;
	B0 = R0;
	R0 = [P2 + 12];
	L0 = R0;
	R5 = [P2 + 16];
	R6 = [P2 + 20];
	R4 = [P2 + 32];
	P1 = [P2 + 36];
	R7.H = 0x03e0;
	R7.L = 0x7c1f;
	R0 = [P2 + 24];
	R2 = [P2 + 28];
	I1 = R2;
	P2 = [P2 + 4];
	CC = R0 == 0;
	IF CC JUMP .Lstail;
	LC0 = R0;
	LSETUP (.Lsm0, .Lsm1) LC0;
.Lsm0:
	R0 = W[P1++P2] (Z);
	R1 = LSHIFT R6 BY R5.L || R3 = [I0] || NOP;
	R1 = R1 & R7;
	R0 = PACK (R0.L, R0.L);
	R0 = R0 & R7;
	R2 = R1 >> 16;
	R1 = R1 | R2;
	R6 = R6 + R0;
	R6 = R6 - R3 (NS) || [I0++] = R0 || NOP;
.Lsm1:
	W[P0++P2] = R1.L;
.Lstail:
	R2 = I1;
	CC = R2 == 0;
	IF CC JUMP .Lsret;
	LC0 = R2;
	LSETUP (.Lst0, .Lst1) LC0;
.Lst0:
	R1 = LSHIFT R6 BY R5.L;
	R1 = R1 & R7;
	R3 = R1 >> 16;
	R1 = R1 | R3;
	R3 = [I0];
	R6 = R6 + R4;
	W[P0++P2] = R1.L;
	R6 = R6 - R3;
.Lst1:
	[I0++] = R4;
.Lsret:
	L0 = 0;
	(R7:4) = [SP++];
	RTS;
	.size _spike_smear_line, .-_spike_smear_line
