// scalar fields for <Dither>: each returns 0..1 per cell, which the component
// quantizes onto a palette with an 8x8 bayer matrix.

export type Field = {
	sample(x: number, y: number, t: number, cols: number, rows: number): number;
	/** flip whatever's under a cell (hero tiles). returns true if something changed. */
	poke?(x: number, y: number, t: number): boolean;
	/** still animating after a poke, even with reduced motion. */
	busy?(t: number): boolean;
};

const hash = (x: number, y: number) => {
	let h = (x * 374761393 + y * 668265263) | 0;
	h = Math.imul(h ^ (h >>> 13), 1274126177);
	return ((h ^ (h >>> 16)) >>> 0) / 4294967295;
};

const smooth = (t: number) => t * t * (3 - 2 * t);

/** value noise, cheap and good enough at dither resolution. */
const noise = (x: number, y: number) => {
	const xi = Math.floor(x),
		yi = Math.floor(y);
	const xf = smooth(x - xi),
		yf = smooth(y - yi);
	const a = hash(xi, yi),
		b = hash(xi + 1, yi),
		c = hash(xi, yi + 1),
		d = hash(xi + 1, yi + 1);
	return a + (b - a) * xf + (c - a) * yf + (a - b - c + d) * xf * yf;
};

/** truchet tiles, the tessel icon, with a glow travelling along the arcs. hover flips tiles. */
export function truchet(tile = 18): Field {
	// tile key -> [time flipped, from orientation]
	const flips = new Map<number, [number, number]>();
	const toggled = new Set<number>();
	const key = (tx: number, ty: number) => tx * 4096 + ty;
	const orient = (tx: number, ty: number) => (hash(tx, ty) > 0.5 ? 1 : 0) ^ (toggled.has(key(tx, ty)) ? 1 : 0);
	const FLIP = 0.45;

	const arcs = (lx: number, ly: number, o: number) => {
		if (o) lx = 1 - lx;
		const d1 = Math.abs(Math.hypot(lx, ly) - 0.5);
		const d2 = Math.abs(Math.hypot(1 - lx, 1 - ly) - 0.5);
		return Math.max(0, 1 - Math.min(d1, d2) / 0.12);
	};

	return {
		sample(x, y, t, cols, rows) {
			const tx = Math.floor(x / tile),
				ty = Math.floor(y / tile);
			const lx = ((x % tile) + 0.5) / tile,
				ly = ((y % tile) + 0.5) / tile;
			const k = key(tx, ty);
			let line = arcs(lx, ly, orient(tx, ty));
			const f = flips.get(k);
			if (f) {
				const p = (t - f[0]) / FLIP;
				if (p >= 1) flips.delete(k);
				else line = line * smooth(p) + arcs(lx, ly, f[1]) * (1 - smooth(p));
			}
			const gx = x / cols,
				gy = y / rows;
			// a sky that's light top-right and sinks to the bottom-left, drifting.
			const sky =
				0.62 * (1 - gy) * 0.8 +
				0.28 * gx +
				0.22 * (noise(gx * 2.2 + t * 0.04, gy * 2.2 - t * 0.03) - 0.5);
			const glow = 0.5 + 0.5 * Math.sin((x + y) * 0.045 - t * 0.9);
			return sky * 0.78 + line * (0.18 + 0.32 * glow);
		},
		poke(x, y, t) {
			const tx = Math.floor(x / tile),
				ty = Math.floor(y / tile);
			const k = key(tx, ty);
			if (flips.has(k)) return false;
			flips.set(k, [t, orient(tx, ty)]);
			if (toggled.has(k)) toggled.delete(k);
			else toggled.add(k);
			return true;
		},
		busy: () => flips.size > 0
	};
}

/** a glow coming up from one corner and sinking into navy, like a field at dusk. */
export function meadow(cx = 0.85, cy = 0.05): Field {
	return {
		sample(x, y, t, cols, rows) {
			const gx = x / cols,
				gy = y / rows;
			const dx = gx - cx - 0.04 * Math.sin(t * 0.2),
				dy = (gy - cy) * 1.25;
			const blob = Math.exp(-(dx * dx + dy * dy) * 2.6);
			const n = noise(gx * 3 + t * 0.05, gy * 3) - 0.5;
			return 0.08 + 0.6 * blob + 0.32 * (1 - gy) * (0.5 + gx * 0.5) + 0.18 * n - 0.15 * gy;
		}
	};
}

/** long diagonal smears, a photo taken from a moving train. */
export function streak(angle = -0.6): Field {
	const ca = Math.cos(angle),
		sa = Math.sin(angle);
	return {
		sample(x, y, t, cols, rows) {
			const u = x * ca - y * sa,
				v = x * sa + y * ca;
			const a = noise(u * 0.012 + t * 0.12, v * 0.16);
			const b = noise(u * 0.03 - t * 0.05, v * 0.4 + 9);
			const gy = y / rows,
				gx = x / cols;
			const light = 0.35 * (1 - gy) + 0.2 * (1 - gx);
			return a * 0.6 + b * 0.25 + light - 0.2;
		}
	};
}

/** a flat wash for small panels. */
export function wash(): Field {
	return {
		sample(x, y, t, cols, rows) {
			const gx = x / cols,
				gy = y / rows;
			return 0.52 - 0.22 * gx + 0.2 * gy + 0.28 * (noise(gx * 4 + t * 0.06, gy * 4 + 3) - 0.5);
		}
	};
}
