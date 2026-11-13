<script lang="ts">
	import Dither from './Dither.svelte';
	import Mark from './Mark.svelte';
	import { truchet, wash } from '../fields';

	const tiles = truchet(18);
	const stage = wash();

	let allowed = $state(false);
</script>

<section class="hero">
	<Dither
		field={tiles}
		cell={7}
		interactive
		palette={['#0b1d22', '#16393d', '#2c6262', '#4f8d86', '#86b9a2', '#c4d8ad', '#ece4c4']}
	/>

	<div class="sheet">
		<nav>
			<a class="brand" href="/" aria-label="Tessel, home"><Mark size={24} /> Tessel</a>
			<ul>
				<li><a class="tag" href="#features">Features</a></li>
				<li><a class="tag" href="#remote">Remote</a></li>
				<li><a class="tag" href="#scripting">Scripting</a></li>
				<li><a class="tag" href="#looks">Themes</a></li>
			</ul>
			<a class="gh tag" href="https://github.com/LeafdTK/tessel">GitHub ↗</a>
		</nav>

		<h1>
			A multiplexer<br /><span class="dim">for coding agents.</span>
		</h1>

		<div class="stage">
			<Dither field={stage} cell={5} grid={0.4} palette={['#2c4a43', '#4f7c6c', '#7fa88c', '#a9c7a2', '#cfdcb4', '#e8e6c6']} />

			<img
				class="window"
				src="/shots/dark.webp"
				alt="Tessel's window: a sidebar of sessions, a conversation with an agent, and its changes beside it."
				width="1600"
				height="1059"
				fetchpriority="high"
			/>

			<div class="notch" role="group" aria-label="An agent asking for permission">
				<div class="notch-head">
					<span>{allowed ? '1 waiting · 4 done' : '2 waiting · 3 done'}</span>
					<span class="notch-mark"><Mark size={16} /></span>
				</div>
				<div class="ask">
					<div>
						<b>Set up a dev venv</b> <span class="dim">tidewater / venv</span>
						<p class:ok={allowed}>
							{allowed ? 'Running · python -m venv .venv' : 'Needs permission · 1m'}
						</p>
					</div>
					{#if !allowed}
						<div class="btns">
							<button type="button" class="ghost">Deny</button>
							<button type="button" onclick={() => (allowed = true)}>Allow</button>
						</div>
					{/if}
				</div>
			</div>
		</div>

		<div class="foot">
			<p class="lede">
				Tessel is a Mac app for running coding agents side by side.
				<span class="dim"
					>Each one gets its own worktree, they keep going when you close the window, and anything you
					can click, you can script.</span
				>
			</p>
			<div class="cta">
				<a class="btn" href="#build">Build it from source</a>
				<a class="new" href="#remote">Coming next: agents on your own Linux machines →</a>
			</div>
		</div>
	</div>
</section>

<style>
	.hero {
		position: relative;
		padding: 120px 0 0 clamp(16px, 9vw, 160px);
	}

	.sheet {
		position: relative;
		background: var(--ink);
		border-radius: 30px 0 0 0;
		padding: 30px clamp(24px, 5vw, 76px) 72px;
		box-shadow: 0 -30px 80px rgba(0, 0, 0, 0.35);
	}

	nav {
		display: flex;
		align-items: center;
		gap: 48px;
		margin-bottom: clamp(48px, 7vw, 96px);
	}
	nav ul {
		display: flex;
		gap: 40px;
		list-style: none;
		margin: 0;
		padding: 0;
	}
	nav a {
		text-decoration: none;
	}
	nav ul a:hover,
	.gh:hover {
		color: var(--bone);
	}
	.brand {
		display: inline-flex;
		align-items: center;
		gap: 10px;
		font-family: var(--display);
		font-weight: 600;
		font-size: 21px;
		letter-spacing: -0.02em;
	}
	.gh {
		margin-left: auto;
	}

	h1 {
		font-size: clamp(44px, 6.6vw, 100px);
		margin-bottom: 52px;
	}

	.stage {
		position: relative;
		height: clamp(440px, 52vw, 680px);
		overflow: hidden;
		isolation: isolate;
	}

	.window {
		position: absolute;
		left: 38%;
		top: 15%;
		width: 82%;
		max-width: none;
		border-radius: 14px;
		box-shadow:
			0 40px 90px rgba(5, 15, 12, 0.5),
			0 0 0 1px rgba(0, 0, 0, 0.4);
	}

	.notch {
		position: absolute;
		top: 9%;
		right: 4%;
		width: min(400px, 46%);
		z-index: 3;
		background: #000;
		color: var(--bone);
		border-radius: 0 0 22px 22px;
		padding: 12px 18px 16px;
		font-size: 14px;
		box-shadow: 0 30px 60px rgba(0, 0, 0, 0.45);
	}
	.notch-head {
		display: flex;
		justify-content: space-between;
		color: var(--stone);
		font-size: 13px;
		margin-bottom: 10px;
	}
	.notch-mark {
		color: var(--iris);
	}
	.ask {
		display: flex;
		gap: 12px;
		align-items: center;
		justify-content: space-between;
	}
	.ask b {
		font-weight: 500;
	}
	.ask .dim {
		font-size: 13px;
	}
	.ask p {
		margin: 2px 0 0;
		color: var(--iris);
		font-size: 13px;
	}
	.ask p.ok {
		color: var(--mint);
	}
	.btns {
		display: flex;
		gap: 6px;
		flex: none;
	}
	button {
		font: inherit;
		font-size: 13.5px;
		border: 0;
		border-radius: 9px;
		padding: 6px 12px;
		background: #e2e2e2;
		color: #000;
		cursor: pointer;
	}
	button.ghost {
		background: #1c1c1c;
		color: var(--bone);
	}


	.foot {
		display: grid;
		grid-template-columns: 1.4fr 1fr;
		gap: 40px;
		align-items: end;
		margin-top: 28px;
	}
	.lede {
		margin: 0;
		font-size: 17px;
		max-width: 52ch;
	}
	.cta {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: 14px;
		justify-self: end;
	}
	.btn {
		text-decoration: none;
		background: var(--bone);
		color: var(--ink);
		font-weight: 550;
		padding: 11px 18px;
		border-radius: 12px;
	}
	.btn:hover {
		background: #fff;
	}
	.new {
		text-decoration: none;
		font-size: 14.5px;
		color: var(--bone);
	}
	.new:hover {
		text-decoration: underline;
		text-underline-offset: 4px;
	}

	@media (max-width: 860px) {
		.hero {
			padding: 72px 12px 0;
		}
		.sheet {
			border-radius: 24px 24px 0 0;
		}
		nav ul {
			display: none;
		}
		.window {
			left: 8%;
			top: 30%;
			width: 150%;
		}
		.notch {
			width: auto;
			left: 4%;
			right: 4%;
			top: auto;
			bottom: 20px;
		}
		.foot {
			grid-template-columns: 1fr;
		}
		.stage {
			height: 420px;
		}
		.cta {
			justify-self: start;
		}
	}
</style>
