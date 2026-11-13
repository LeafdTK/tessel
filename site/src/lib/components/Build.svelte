<script lang="ts">
	import Mark from './Mark.svelte';
	import Dither from './Dither.svelte';
	import { truchet } from '../fields';

	const tiles = truchet(18);

	const steps = `export PATH="$(brew --prefix zig@0.15)/bin:$PATH"
cargo build --release -p tessel -p tesseld -p tesselctl
scripts/bundle-macos.sh release
open target/release/Tessel.app`;

	let copied = $state(false);
	async function copy() {
		await navigator.clipboard.writeText(steps);
		copied = true;
		setTimeout(() => (copied = false), 1600);
	}
</script>

<section id="build" class="wrap">
	<div class="words">
		<h2>Install</h2>
		<p class="lede">
			Build it from <a href="https://github.com/LeafdTK/tessel">the source on GitHub</a>. You need:
		</p>
		<ul class="needs">
			<li>macOS 13 or later</li>
			<li>Rust, the version pinned in <code>rust-toolchain.toml</code></li>
			<li>zig 0.15 for the terminal: <code>brew install zig@0.15</code></li>
			<li>The Claude Code CLI, logged in</li>
		</ul>
	</div>

	<div class="term">
		<div class="term-bar">
			<span class="tag">zsh · tessel</span>
			<button type="button" class="tag" onclick={copy}>{copied ? 'Copied' : 'Copy'}</button>
		</div>
		<pre><code>{#each steps.split('\n') as line, i (i)}<span class="p">$</span> {line}
{/each}</code></pre>
		<p class="warn">The first build needs several GB of disk.</p>
	</div>
</section>

<footer class="wrap">
	<a class="brand" href="https://github.com/LeafdTK/tessel"><Mark size={20} /> Tessel</a>
	<p>
		Built in Rust on <a href="https://www.gpui.rs">gpui</a>, on top of
		<a href="https://docs.claude.com/en/docs/claude-code">Claude Code</a>.
	</p>
	<a class="tag gh" href="https://github.com/LeafdTK/tessel">github.com/LeafdTK/tessel ↗</a>
</footer>

<div class="tiles">
	<Dither
		field={tiles}
		cell={7}
		interactive
		palette={['#0b1d22', '#16393d', '#2c6262', '#4f8d86', '#86b9a2', '#c4d8ad', '#ece4c4']}
	/>
</div>

<style>
	#build {
		display: grid;
		grid-template-columns: 1fr minmax(0, 600px);
		gap: 56px;
		align-items: start;
		padding: 120px 0 140px;
		border-top: 1px solid var(--rule);
	}
	h2 {
		font-size: clamp(34px, 4vw, 52px);
		margin-bottom: 20px;
	}
	.lede {
		margin: 0 0 28px;
		color: var(--stone);
		font-size: 17px;
	}
	.lede a {
		color: var(--bone);
		text-underline-offset: 3px;
	}
	.needs {
		list-style: none;
		margin: 0;
		padding: 0;
		color: var(--bone);
	}
	.needs li {
		display: flex;
		align-items: baseline;
		gap: 14px;
		padding: 10px 0;
		border-top: 1px solid var(--rule);
	}
	.needs code {
		font-size: 13px;
		color: var(--bone);
	}

	.term {
		background: var(--coal);
		border: 1px solid var(--rule);
		border-radius: var(--radius);
		overflow: hidden;
	}
	.term-bar {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 14px 18px;
		border-bottom: 1px solid var(--rule);
	}
	.term-bar button {
		background: none;
		border: 1px solid var(--rule);
		border-radius: 8px;
		padding: 5px 10px;
		cursor: pointer;
	}
	.term-bar button:hover {
		color: var(--bone);
	}
	pre {
		margin: 0;
		padding: 20px 18px 8px;
		overflow-x: auto;
		font-size: 13.5px;
		line-height: 1.9;
	}
	.p {
		color: var(--ash);
		user-select: none;
	}
	.warn {
		margin: 0;
		padding: 0 18px 18px;
		color: var(--amber);
		font-size: 14px;
	}

	footer {
		display: flex;
		align-items: center;
		gap: 28px;
		padding: 28px 0 48px;
		border-top: 1px solid var(--rule);
		color: var(--stone);
		font-size: 14.5px;
	}
	footer p {
		margin: 0;
	}
	footer p a {
		color: var(--bone);
		text-underline-offset: 3px;
	}
	.brand {
		display: inline-flex;
		align-items: center;
		gap: 9px;
		color: var(--bone);
		text-decoration: none;
		font-family: var(--display);
		font-weight: 600;
		font-size: 18px;
	}
	.gh {
		margin-left: auto;
		text-decoration: none;
	}
	.gh:hover {
		color: var(--bone);
	}

	.tiles {
		position: relative;
		height: 180px;
		mask-image: linear-gradient(transparent, #000 70%);
		-webkit-mask-image: linear-gradient(transparent, #000 70%);
	}

	@media (max-width: 900px) {
		#build {
			grid-template-columns: 1fr;
		}
		footer {
			flex-wrap: wrap;
		}
		.gh {
			margin-left: 0;
		}
	}
</style>
