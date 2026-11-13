<script lang="ts">
	import { onMount } from 'svelte';
	import type { Field } from '../fields';

	type Props = {
		/** dark to light. */
		palette: string[];
		field: Field;
		/** css px per dither pixel. */
		cell?: number;
		/** how much of the dot grid shows between pixels, 0..1. */
		grid?: number;
		fps?: number;
		/** hovering flips the field (if it can be poked). */
		interactive?: boolean;
	};

	let { palette, field, cell = 6, grid = 0.55, fps = 14, interactive = false }: Props = $props();

	// prettier-ignore
	const BAYER = [
		0, 32, 8, 40, 2, 34, 10, 42, 48, 16, 56, 24, 50, 18, 58, 26,
		12, 44, 4, 36, 14, 46, 6, 38, 60, 28, 52, 20, 62, 30, 54, 22,
		3, 35, 11, 43, 1, 33, 9, 41, 51, 19, 59, 27, 49, 17, 57, 25,
		15, 47, 7, 39, 13, 45, 5, 37, 63, 31, 55, 23, 61, 29, 53, 21
	].map((v) => (v + 0.5) / 64);

	let wrap: HTMLDivElement;
	let canvas: HTMLCanvasElement;

	const rgb = (hex: string) => {
		const n = parseInt(hex.slice(1), 16);
		return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
	};

	onMount(() => {
		const ctx = canvas.getContext('2d', { alpha: false })!;
		const colors = palette.map(rgb);
		const top = colors.length - 1;
		const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
		const t0 = performance.now();
		const now = () => (performance.now() - t0) / 1000;

		let cols = 0,
			rows = 0,
			img: ImageData | null = null,
			visible = false,
			raf = 0,
			last = -1;

		const draw = (t: number) => {
			if (!img) return;
			const d = img.data;
			let k = 0;
			for (let y = 0; y < rows; y++) {
				const by = (y & 7) * 8;
				for (let x = 0; x < cols; x++) {
					let v = field.sample(x, y, t, cols, rows);
					v = v < 0 ? 0 : v > 1 ? 1 : v;
					const s = v * top;
					let i = s | 0;
					if (s - i > BAYER[by + (x & 7)]) i++;
					const c = colors[i > top ? top : i];
					d[k] = c[0];
					d[k + 1] = c[1];
					d[k + 2] = c[2];
					d[k + 3] = 255;
					k += 4;
				}
			}
			ctx.putImageData(img, 0, 0);
		};

		const resize = () => {
			cols = Math.max(1, Math.ceil(wrap.clientWidth / cell));
			rows = Math.max(1, Math.ceil(wrap.clientHeight / cell));
			canvas.width = cols;
			canvas.height = rows;
			canvas.style.width = `${cols * cell}px`;
			canvas.style.height = `${rows * cell}px`;
			img = ctx.createImageData(cols, rows);
			draw(still ? 0 : now());
		};

		const tick = (ms: number) => {
			raf = 0;
			if (!visible) return;
			const t = now();
			const animate = !still || field.busy?.(t);
			if (ms - last >= 1000 / fps) {
				last = ms;
				draw(still ? (field.busy?.(t) ? t : 0) : t);
			}
			if (animate) raf = requestAnimationFrame(tick);
		};
		const kick = () => {
			if (!raf && visible) raf = requestAnimationFrame(tick);
		};

		const ro = new ResizeObserver(resize);
		ro.observe(wrap);
		const io = new IntersectionObserver(([e]) => {
			visible = e.isIntersecting;
			if (visible) kick();
		});
		io.observe(wrap);

		const onMove = (e: PointerEvent) => {
			if (!field.poke) return;
			const r = canvas.getBoundingClientRect();
			const x = Math.floor((e.clientX - r.left) / cell),
				y = Math.floor((e.clientY - r.top) / cell);
			if (x < 0 || y < 0 || x >= cols || y >= rows) return;
			if (field.poke(x, y, now())) kick();
		};
		if (interactive) window.addEventListener('pointermove', onMove, { passive: true });

		return () => {
			ro.disconnect();
			io.disconnect();
			cancelAnimationFrame(raf);
			if (interactive) window.removeEventListener('pointermove', onMove);
		};
	});
</script>

<div class="dither" bind:this={wrap} style:--cell="{cell}px" style:--grid={grid} aria-hidden="true">
	<canvas bind:this={canvas}></canvas>
</div>

<style>
	.dither {
		position: absolute;
		inset: 0;
		overflow: hidden;
		background: var(--ink);
	}
	canvas {
		display: block;
		image-rendering: pixelated;
	}
	/* the gaps between dots, so it reads as printed rather than upscaled */
	.dither::after {
		content: '';
		position: absolute;
		inset: 0;
		pointer-events: none;
		background-image:
			linear-gradient(90deg, var(--ink) 1px, transparent 1px),
			linear-gradient(var(--ink) 1px, transparent 1px);
		background-size: var(--cell) var(--cell);
		opacity: var(--grid);
	}
</style>
