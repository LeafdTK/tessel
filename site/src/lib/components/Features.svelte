<script lang="ts">
	const items = [
		{
			name: 'Parallel agents',
			key: '⌘N',
			body: 'Each new session gets its own worktree and branch. Sessions waiting on you move to the top of the sidebar, and ⌘J jumps to the desktop that needs you.'
		},
		{
			name: 'Worktree setup',
			body: 'New worktrees get your .env files, run your setup script and get ten ports of their own in $TESSEL_PORT. Scripts live in .tessel/settings.toml.'
		},
		{
			name: 'Checkpoints',
			body: 'Every message takes one. Undo a turn, or fork from any point into a new tab or worktree.'
		},
		{
			name: 'Shipping',
			body: 'Open a PR, squash or rebase it in, or squash straight into main. Tessel never makes a merge commit. Conflicts go back to the agent with one click.'
		},
		{
			name: 'Panes',
			body: "A terminal on Ghostty's core, a git graph with line staging, diffs, files, PR checks and a Kubernetes view. Split them however you like."
		},
		{
			name: 'Desktops',
			key: '⌘1–9',
			body: 'A separate layout for each project. Switch with the keyboard or swipe on the sidebar.'
		},
		{
			name: 'Command palette',
			key: '⌘K',
			body: 'Every action, session and pane. Usage limits show next to the message box.'
		},
		{
			name: 'Restarts',
			body: 'tesselctl daemon restart hands terminals and idle agents to the new build with their scrollback. Agents in the middle of a turn finish first.'
		}
	];

	const bash = [
		'$ cargo test -p tesseld',
		'   Compiling tesseld v0.3.0',
		'running 214 tests',
		'test hub::handover_keeps_terminals ... ok',
		'test term::resize_reflows ... ok',
		'test projects::made_by_daemon ... ok',
		'test result: ok. 214 passed; 0 failed',
		'$ git diff --stat',
		' crates/tesseld/src/hub.rs | 16 ++++'
	];
</script>

<section id="features" class="wrap">
	<h2>Features</h2>

	<figure class="lead">
		<img
			src="/shots/tiles.webp"
			alt="A conversation with an agent, a terminal and the git graph side by side."
			width="1600"
			height="1059"
			loading="lazy"
		/>
		<figcaption>A conversation, its terminal and the git graph in one layout.</figcaption>
	</figure>

	<dl>
		{#each items as it (it.name)}
			<div>
				<dt>{it.name}{#if it.key}<kbd>{it.key}</kbd>{/if}</dt>
				<dd>{it.body}</dd>
			</div>
		{/each}
	</dl>

	<div class="figs">
		<figure>
			<div class="frame pick" aria-hidden="true">
				<div class="page">
					<div class="pg-bar"><i></i><i></i><i></i><span>localhost:5173</span></div>
					<div class="pg-body">
						<b class="ln" style:width="62%"></b>
						<b class="ln" style:width="44%"></b>
						<span class="picked">Get forecast<em>button.cta</em></span>
					</div>
				</div>
				<div class="attach">
					<code>&lt;button class="cta"&gt;</code>
					<span class="src">src/routes/+page.svelte:42</span>
				</div>
			</div>
			<figcaption>
				<b>Browser.</b> A WebKit page in a pane that the agent can click around in while you watch. Pick
				an element and it's attached to your next message, with its source file on dev builds.
			</figcaption>
		</figure>

		<figure>
			<div class="frame notch">
				<img
					src="/shots/notch.webp"
					alt="The notch, open: two agents waiting, three done, with Allow and Deny buttons."
					width="1240"
					height="676"
					loading="lazy"
				/>
			</div>
			<figcaption>
				<b>The notch.</b> On a notched MacBook, hover the notch to see every agent and allow or deny
				requests without switching apps.
			</figcaption>
		</figure>

		<figure>
			<div class="frame tail" aria-hidden="true">
				<div class="roll">
					{#each [...bash, ...bash] as l, i (i)}
						<div class:cmd={l.startsWith('$')} class:ok={l.includes('ok')}>{l}</div>
					{/each}
				</div>
			</div>
			<figcaption>
				<b>Visual mode.</b> Follow what the agent is doing as it happens, including its own shell output.
			</figcaption>
		</figure>
	</div>
</section>

<style>
	section {
		padding: 140px 0 120px;
	}
	h2 {
		font-size: clamp(34px, 4vw, 52px);
		margin-bottom: 48px;
	}

	.lead {
		margin: 0 0 72px;
	}
	.lead img {
		border-radius: 14px;
		box-shadow: 0 0 0 1px var(--rule);
	}
	figcaption {
		margin-top: 14px;
		color: var(--stone);
		font-size: 15px;
		max-width: 60ch;
	}
	figcaption b {
		color: var(--bone);
		font-weight: 550;
	}

	dl {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 36px 64px;
		margin: 0 0 96px;
	}
	dt {
		display: flex;
		align-items: baseline;
		gap: 12px;
		font-family: var(--display);
		font-weight: 600;
		font-size: 20px;
		letter-spacing: -0.02em;
		margin-bottom: 6px;
	}
	kbd {
		font-size: 12.5px;
		color: var(--stone);
		font-weight: 400;
		letter-spacing: 0;
	}
	dd {
		margin: 0;
		color: var(--stone);
		max-width: 52ch;
	}

	.figs {
		display: grid;
		grid-template-columns: 1.1fr 1.2fr 0.9fr;
		gap: 32px;
		align-items: start;
	}
	figure {
		margin: 0;
	}
	.frame {
		border-radius: 16px;
		background: var(--coal);
		padding: 22px;
		min-height: 260px;
	}
	.notch {
		display: grid;
		place-items: center;
		background: #e9e6e0;
		padding: 18px 12px;
	}

	.page {
		border-radius: 10px;
		background: #f1eee8;
		color: #1b1b1b;
		overflow: hidden;
	}
	.pg-bar {
		display: flex;
		align-items: center;
		gap: 5px;
		padding: 8px 12px;
		background: #e3dfd7;
		font-family: var(--mono);
		font-size: 11px;
		color: #6c675e;
	}
	.pg-bar i {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: #c8c2b6;
	}
	.pg-bar span {
		margin-left: 10px;
	}
	.pg-body {
		padding: 18px 18px 22px;
		display: grid;
		gap: 9px;
		justify-items: start;
	}
	.ln {
		height: 8px;
		border-radius: 4px;
		background: #d9d4ca;
	}
	.picked {
		position: relative;
		margin-top: 8px;
		padding: 8px 14px;
		border-radius: 9px;
		background: #1b1b1b;
		color: #f1eee8;
		font-size: 14px;
		outline: 2px dashed var(--iris);
		outline-offset: 4px;
	}
	.picked em {
		position: absolute;
		left: calc(100% + 14px);
		top: 50%;
		transform: translateY(-50%);
		font-style: normal;
		font-family: var(--mono);
		font-size: 11px;
		padding: 3px 7px;
		border-radius: 6px;
		background: var(--iris);
		color: #0c1020;
		white-space: nowrap;
	}
	.attach {
		display: flex;
		flex-wrap: wrap;
		gap: 4px 10px;
		margin-top: 12px;
		padding: 10px 14px;
		border-radius: 10px;
		background: rgba(147, 166, 242, 0.08);
		border: 1px solid rgba(147, 166, 242, 0.18);
		font-size: 13px;
	}
	.attach .src {
		color: var(--stone);
		font-family: var(--mono);
		font-size: 12px;
	}

	.tail {
		height: 260px;
		overflow: hidden;
		background: #050606;
		box-shadow: inset 0 0 0 1px var(--rule);
		padding: 16px 16px 0;
		font-family: var(--mono);
		font-size: 12px;
		line-height: 1.8;
		color: var(--stone);
		mask-image: linear-gradient(transparent, #000 25%);
		-webkit-mask-image: linear-gradient(transparent, #000 25%);
	}
	.roll {
		animation: roll 14s steps(9) infinite;
	}
	.roll div {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.roll .cmd {
		color: var(--bone);
	}
	.roll .ok {
		color: var(--mint);
	}
	@keyframes roll {
		to {
			transform: translateY(-50%);
		}
	}

	@media (max-width: 960px) {
		.figs {
			grid-template-columns: 1fr;
		}
	}
	@media (max-width: 640px) {
		section {
			padding: 100px 0 80px;
		}
		dl {
			grid-template-columns: 1fr;
			gap: 28px;
		}
	}
</style>
