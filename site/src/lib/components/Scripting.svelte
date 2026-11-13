<script lang="ts">
	import { onMount } from 'svelte';
	import Dither from './Dither.svelte';
	import Mark from './Mark.svelte';
	import { streak } from '../fields';

	const field = streak(-0.62);

	const commands = [
		{
			name: 'new',
			line: 'tesselctl new --worktree --model sonnet "add a --json flag to the forecast command"',
			does: 'Starts an agent in a fresh worktree, on its own branch.'
		},
		{ name: 'ls', line: 'tesselctl ls', does: 'Lists your sessions and what each one is doing.' },
		{ name: 'approve', line: 'tesselctl approve 4f7b', does: 'Allows what an agent is asking for, from any terminal.' },
		{ name: 'ui', line: 'tesselctl ui layout.use name=tiles', does: 'Switches the window to the tiles layout. Panes and desktops work the same way.' },
		{ name: 'term run', line: 'tesselctl term run 2 "cargo test"', does: 'Runs a command in a terminal pane.' },
		{ name: 'open', line: 'EDITOR="tesselctl open" git commit', does: 'Opens files in Tessel, so it can be your $EDITOR.' },
		{ name: 'host use', line: 'tesselctl host use devbox', does: 'Points the window at another machine’s daemon.' }
	];

	let picked = $state(0);
	const cmd = $derived(commands[picked]);

	const asks = [
		'Ask, or tell the agent what to do…',
		'add a --json flag to the forecast command',
		'forecasts never refresh, find out why',
		'run the tests on devbox and fix what fails'
	];
	let typed = $state(asks[0]);

	onMount(() => {
		if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;
		let a = 0,
			n = asks[0].length,
			dir = -1,
			wait = 40;
		const id = setInterval(() => {
			if (wait > 0) return void wait--;
			n += dir;
			if (n <= 0) {
				a = (a + 1) % asks.length;
				dir = 1;
				n = 0;
			} else if (n >= asks[a].length) {
				dir = -1;
				wait = 46;
			}
			typed = asks[a].slice(0, n);
		}, 45);
		return () => clearInterval(id);
	});
</script>

<section id="scripting">
	<Dither
		{field}
		cell={6}
		grid={0.5}
		palette={['#050704', '#0f170c', '#1d2c15', '#33461f', '#55692d', '#86973f', '#bfc27a', '#e3dcb0']}
	/>

	<div class="scrim"></div>
	<div class="inner wrap">
		<div class="words">
			<h2>Scripting</h2>
			<p class="lede">
				The window, <code>tesselctl</code> and the agents all talk to the same daemon, so anything you
				can do in the window you can do from a terminal. Agents get the same commands as MCP tools.
			</p>
		</div>

		<div class="side">
			<div class="composer">
				<div class="who">
					<span class="avatar"><Mark size={18} /></span>
					<span><b>Tessel:</b> <span class="dim2">Add a --json flag to forecast</span></span>
				</div>
				<p class="input" aria-label="Message box, showing example prompts">
					<span class:ph={typed === asks[0] || typed.length === 0}>{typed}</span><span class="cursor"></span>
				</p>
				<div class="row">
					<span class="chip"><i class="m">S</i> Sonnet 5.5 <span class="chev">⌄</span></span>
					<span class="chip">
						<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"
							><path d="M9 1.5 3.5 9H8l-1 5.5L12.5 7H8z" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linejoin="round" /></svg
						>
						Auto <span class="chev">⌄</span>
					</span>
				</div>
			</div>

			<p class="or">From a terminal:</p>
			<div class="cmds" role="listbox" aria-label="tesselctl commands">
				{#each commands as c, i (c.name)}
					<button type="button" role="option" aria-selected={picked === i} class:on={picked === i} onclick={() => (picked = i)}>
						<span class="br">&lt;&gt;</span> {c.name}
					</button>
				{/each}
			</div>

			<div class="out" aria-live="polite">
				<code><span class="dollar">$</span> {cmd.line}</code>
				<p>{cmd.does}</p>
			</div>
		</div>
	</div>
</section>

<style>
	section {
		position: relative;
		padding: clamp(80px, 11vw, 160px) 0;
	}
	.scrim {
		position: absolute;
		inset: 0;
		background:
			linear-gradient(90deg, rgba(5, 7, 4, 0.82) 0%, rgba(5, 7, 4, 0.55) 38%, rgba(5, 7, 4, 0) 62%),
			linear-gradient(rgba(7, 8, 10, 1), rgba(7, 8, 10, 0) 18%, rgba(7, 8, 10, 0) 82%, rgba(7, 8, 10, 1));
	}
	.inner {
		position: relative;
		display: grid;
		grid-template-columns: 1fr minmax(0, 560px);
		gap: 56px;
		align-items: center;
	}

	h2 {
		font-size: clamp(34px, 4vw, 52px);
		margin-bottom: 20px;
		text-shadow: 0 2px 30px rgba(0, 0, 0, 0.5);
	}
	.lede code {
		font-size: 0.88em;
		background: rgba(0, 0, 0, 0.35);
		padding: 1px 6px;
		border-radius: 6px;
	}
	.lede {
		margin: 0;
		font-size: 18px;
		max-width: 40ch;
		text-shadow: 0 1px 20px rgba(0, 0, 0, 0.7);
	}

	.composer {
		background: rgba(14, 16, 13, 0.82);
		backdrop-filter: blur(20px);
		-webkit-backdrop-filter: blur(20px);
		border: 1px solid rgba(255, 255, 255, 0.06);
		border-radius: 24px;
		padding: 22px 26px 18px;
		box-shadow: 0 40px 100px rgba(0, 0, 0, 0.45);
	}
	.who {
		display: flex;
		align-items: center;
		gap: 12px;
		font-size: 15.5px;
	}
	.who b {
		font-weight: 500;
	}
	.dim2 {
		color: var(--stone);
	}
	.avatar {
		display: grid;
		place-items: center;
		width: 34px;
		height: 34px;
		border-radius: 50%;
		color: #23262b;
		background: radial-gradient(circle at 35% 30%, #fffaf0, #d9d1c3 60%, #a59c8d);
	}
	.input {
		margin: 20px 0 18px;
		padding-bottom: 18px;
		border-bottom: 1px solid var(--rule);
		font-size: clamp(19px, 2.2vw, 26px);
		letter-spacing: -0.01em;
		min-height: 1.5em;
		display: flex;
		align-items: center;
	}
	.input .ph {
		color: var(--stone);
	}
	.cursor {
		display: inline-block;
		width: 2px;
		height: 1.1em;
		background: var(--bone);
		margin-left: 2px;
		animation: blink 1.1s steps(1) infinite;
	}
	@keyframes blink {
		50% {
			opacity: 0;
		}
	}
	.row {
		display: flex;
		gap: 22px;
		color: var(--bone);
		font-size: 15px;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 8px;
	}
	.chip .m {
		font-style: normal;
		font-size: 11px;
		font-weight: 600;
		display: grid;
		place-items: center;
		width: 20px;
		height: 20px;
		border-radius: 5px;
		background: rgba(255, 255, 255, 0.1);
	}
	.chev {
		color: var(--stone);
	}

	.or {
		margin: 32px 0 14px 4px;
		font-size: 16px;
		color: #dcd8c6;
		text-shadow: 0 1px 12px rgba(0, 0, 0, 0.8);
	}
	.cmds {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}
	.cmds button {
		font: inherit;
		font-family: var(--body);
		font-size: 15px;
		color: var(--bone);
		background: rgba(12, 14, 11, 0.62);
		backdrop-filter: blur(8px);
		-webkit-backdrop-filter: blur(8px);
		border: 1px solid rgba(255, 255, 255, 0.05);
		border-radius: 10px;
		padding: 6px 13px 7px 10px;
		cursor: pointer;
	}
	.cmds button:hover {
		background: rgba(30, 34, 26, 0.8);
	}
	.cmds button.on {
		background: var(--bone);
		color: var(--ink);
	}
	.br {
		font-family: var(--mono);
		font-size: 12px;
		opacity: 0.6;
		margin-right: 4px;
	}

	.out {
		margin-top: 14px;
		padding: 16px 18px;
		border-radius: 14px;
		background: rgba(5, 6, 5, 0.78);
		border: 1px solid rgba(255, 255, 255, 0.05);
	}
	.out code {
		display: block;
		font-size: 13.5px;
		line-height: 1.6;
		overflow-wrap: anywhere;
	}
	.dollar {
		color: var(--ash);
	}
	.out p {
		margin: 8px 0 0;
		color: var(--stone);
		font-size: 14.5px;
	}

	@media (max-width: 900px) {
		.inner {
			grid-template-columns: 1fr;
		}
	}
</style>
