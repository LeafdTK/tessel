<script lang="ts">
	import Dither from './Dither.svelte';
	import { meadow } from '../fields';

	const field = meadow(0.82, 0.02);

	type Kind = 'step' | 'note' | 'ok' | 'warn';
	type Line = { kind: Kind; text: string };
	type Host = {
		id: string;
		label: string;
		link: string;
		cmd: string;
		lines: Line[];
		chart?: { title: string; range: string; rtt: number[]; spike?: [number, number]; marks: [string, string, string] };
		after: Line[];
		prompt: string;
	};

	// devbox: wifi, then a few rough samples while the laptop changes networks, then the hotspot.
	const devboxRtt = [
		14, 13, 15, 14, 16, 13, 14, 15, 13, 14, 17, 14, 13, 15, 14, 13, 16, 14, 15, 14, 13, 15, 52, 64, 47,
		31, 26, 25, 27, 24, 26, 25, 27, 26, 24, 25, 26, 27, 25, 26
	];
	const gpuRtt = [
		38, 39, 37, 40, 38, 41, 39, 38, 37, 39, 40, 38, 39, 41, 38, 37, 39, 38, 40, 39, 38, 37, 39, 40, 38,
		39, 41, 38, 39, 37, 38, 40, 39, 38, 37, 39, 38, 40, 39, 38
	];

	const hosts: Host[] = [
		{
			id: 'devbox',
			label: 'devbox',
			link: 'quic · 26 ms',
			cmd: '$ tesselctl host setup devbox',
			lines: [
				{ kind: 'step', text: 'Reaching you@devbox over ssh' },
				{ kind: 'step', text: 'Copying tessel to devbox (linux x86_64)' },
				{ kind: 'note', text: 'tesseld, 38.2 MB · tesselctl, 9.6 MB' },
				{ kind: 'step', text: 'Letting this Mac in' },
				{ kind: 'step', text: "Starting tessel's service" },
				{ kind: 'step', text: 'Installing Claude Code' },
				{ kind: 'step', text: 'Connecting over QUIC' },
				{ kind: 'ok', text: 'devbox is ready: quic to 10.0.4.12:7731' }
			],
			chart: {
				title: 'rtt · devbox · last 30 min',
				range: 'wifi → hotspot · 0 reconnects',
				rtt: devboxRtt,
				spike: [22, 25],
				marks: ['11:20', '11:41', '11:50']
			},
			after: [
				{ kind: 'ok', text: '6 agents and 4 terminals stayed attached the whole way' },
				{ kind: 'note', text: '→ localhost:3000 on devbox opens in the browser pane' }
			],
			prompt: 'tesselctl host use devbox'
		},
		{
			id: 'gpu',
			label: 'gpu-box',
			link: 'ssh · 38 ms',
			cmd: '$ tesselctl host setup gpu-box',
			lines: [
				{ kind: 'step', text: 'Reaching gpu over ssh' },
				{ kind: 'step', text: 'Copying tessel to gpu-box (linux aarch64)' },
				{ kind: 'note', text: 'already up to date' },
				{ kind: 'step', text: 'Letting this Mac in' },
				{ kind: 'step', text: "Starting tessel's service" },
				{ kind: 'step', text: 'Connecting over QUIC' },
				{
					kind: 'warn',
					text: 'no answer on udp 7731. Using ssh until it’s open in the host’s firewall.'
				},
				{ kind: 'step', text: 'Connecting over ssh' },
				{ kind: 'ok', text: 'gpu-box is ready: through ssh' }
			],
			chart: {
				title: 'rtt · gpu-box over ssh · last 30 min',
				range: 'ssh · 38 ms',
				rtt: gpuRtt,
				marks: ['14:10', '', '14:40']
			},
			after: [
				{ kind: 'ok', text: 'same daemon protocol, so every feature works through ssh too' },
				{ kind: 'note', text: '→ open udp 7731 and run setup again for quic' }
			],
			prompt: 'tesselctl host use gpu-box'
		},
		{
			id: 'mac',
			label: 'this mac',
			link: 'local',
			cmd: '$ tesselctl host ls',
			lines: [
				{ kind: 'note', text: '* this mac' },
				{ kind: 'note', text: '  devbox  (devbox, ssh you@devbox; quic 10.0.4.12:7731)' },
				{ kind: 'note', text: '  gpu     (gpu-box, ssh gpu; through ssh)' }
			],
			after: [
				{ kind: 'step', text: '$ tesselctl host use devbox' },
				{ kind: 'ok', text: 'the window shows devbox' }
			],
			prompt: 'tesselctl host use mac'
		}
	];

	let current = $state('devbox');
	const host = $derived(hosts.find((h) => h.id === current)!);

	const glyph: Record<Kind, string> = { step: '·', note: '', ok: '✓', warn: '▲' };

	const facts = [
		{
			title: 'Streams',
			body: "One QUIC connection per host, on UDP 7731. The daemon and each forwarded port get their own stream, so a terminal printing a lot of output doesn't slow the rest down."
		},
		{
			title: 'Changing networks',
			body: 'When your laptop moves from wifi to a phone hotspot, the connection moves with it. Sessions and terminals stay attached.'
		},
		{
			title: 'Keys',
			body: "Each machine has an ed25519 key. Setup reads the host's fingerprint over ssh and pins it, and the host only accepts the Macs on its list."
		},
		{
			title: 'Ports',
			body: "localhost:3000 on the host is localhost:3000 on your Mac, so a dev server running there opens in Tessel's browser pane."
		},
		{
			title: 'ssh fallback',
			body: 'If a firewall blocks UDP, Tessel connects through ssh instead. It takes longer to connect and drops when the network changes, but everything else works the same.'
		}
	];

	function onTabKey(e: KeyboardEvent) {
		const i = hosts.findIndex((h) => h.id === current);
		const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
		if (!step) return;
		e.preventDefault();
		current = hosts[(i + step + hosts.length) % hosts.length].id;
		(document.getElementById(`tab-${current}`) as HTMLElement | null)?.focus();
	}
</script>

<section id="remote">
	<header class="wrap">
		<h2>Remote hosts <span class="soon">in progress</span></h2>
		<p class="lede">
			Run agents on a Linux machine and drive them from the same window. Point Tessel at anything ssh
			can reach and <code>tesselctl host setup</code> copies Tessel over, adds your Mac to the host's
			allowed keys, starts the service and installs Claude Code. Everything in Tessel already goes
			through the daemon, so all of it works on the remote machine too.
		</p>
	</header>

	<div class="band">
		<Dither
			{field}
			cell={7}
			palette={['#04060c', '#0a1430', '#0f2b4a', '#16505a', '#2f7d5a', '#7eb04a', '#c6d94e', '#e6ec86']}
		/>

		<div class="console">
			<div class="bar">
				<div class="tabs" role="tablist" aria-label="Hosts" tabindex="-1" onkeydown={onTabKey}>
					{#each hosts as h (h.id)}
						<button
							type="button"
							role="tab"
							id="tab-{h.id}"
							aria-selected={current === h.id}
							aria-controls="host-panel"
							tabindex={current === h.id ? 0 : -1}
							class:on={current === h.id}
							onclick={() => (current = h.id)}
						>
							{#if h.id === 'mac'}
								<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"
									><rect x="2" y="3" width="12" height="8" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.5" /><path
										d="M1 13.5h14"
										stroke="currentColor"
										stroke-width="1.5"
										stroke-linecap="round"
									/></svg
								>
							{:else}
								<svg width="14" height="14" viewBox="0 0 16 16" aria-hidden="true"
									><rect x="2" y="2.5" width="12" height="4.5" rx="1.2" fill="none" stroke="currentColor" stroke-width="1.5" /><rect
										x="2"
										y="9"
										width="12"
										height="4.5"
										rx="1.2"
										fill="none"
										stroke="currentColor"
										stroke-width="1.5"
									/></svg
								>
							{/if}
							{h.label}
						</button>
					{/each}
				</div>
				<span class="link"><i></i>{host.link}</span>
			</div>

			<div class="body" id="host-panel" role="tabpanel" aria-labelledby="tab-{current}">
				{#key current}
					<p class="cmd tag">{host.cmd}</p>
					<ol class="log">
						{#each host.lines as l, i (i)}
							<li class={l.kind} style:--i={i}>
								<span class="g">{glyph[l.kind]}</span><span class="t">{l.text}</span>
							</li>
						{/each}
					</ol>

					{#if host.chart}
						{@const c = host.chart}
						<figure class="chart" style:--i={host.lines.length}>
							<figcaption>
								<span>{c.title}</span>
								<span class="tag range" class:calm={!c.spike}>{c.range}</span>
							</figcaption>
							<div class="bars">
								{#each c.rtt as v, i (i)}
									{@const spike = c.spike && i >= c.spike[0] && i <= c.spike[1]}
									{@const after = c.spike && i > c.spike[1]}
									<span
										class:spike
										class:after
										class:ssh={!c.spike}
										style:--h="{Math.max(6, (v / 70) * 100)}%"
										style:--j={i}
									></span>
								{/each}
								{#if c.spike}
									<em class="tag mark" style:left="{(c.spike[0] / c.rtt.length) * 100}%">{c.marks[1]}</em>
								{/if}
							</div>
							<div class="axis tag">
								<span>{c.marks[0]}</span><span>{c.marks[2]}</span>
							</div>
						</figure>
					{/if}

					<ul class="after" style:--i={host.lines.length + (host.chart ? 2 : 0)}>
						{#each host.after as l, i (i)}
							<li class={l.kind}><span class="g">{glyph[l.kind]}</span>{l.text}</li>
						{/each}
					</ul>
				{/key}
			</div>

			<div class="prompt">
				<span class="caret">›</span>
				<span class="typed">{host.prompt}</span><span class="cursor" aria-hidden="true"></span>
				<span class="keys tag">← → hosts</span>
			</div>
		</div>
	</div>

	<div class="facts wrap">
		{#each facts as f (f.title)}
			<div>
				<h3>{f.title}</h3>
				<p>{f.body}</p>
			</div>
		{/each}
	</div>
</section>

<style>
	section {
		padding: 40px 0 130px;
	}
	header {
		margin-bottom: 64px;
	}
	.soon {
		font-family: var(--body);
		font-size: 15px;
		font-weight: 500;
		letter-spacing: 0;
		color: #c6d94e;
		vertical-align: middle;
		margin-left: 10px;
		padding: 3px 10px;
		border: 1px solid rgba(198, 217, 78, 0.35);
		border-radius: 99px;
	}
	.lede code {
		font-size: 0.88em;
		background: rgba(236, 230, 219, 0.07);
		padding: 1px 6px;
		border-radius: 6px;
	}
	h2 {
		font-size: clamp(34px, 4vw, 52px);
		margin-bottom: 22px;
	}
	.lede {
		margin: 0;
		font-size: 18px;
		max-width: 60ch;
	}

	.band {
		position: relative;
		padding: clamp(60px, 9vw, 130px) 20px;
		display: grid;
		place-items: center;
	}

	.console {
		position: relative;
		width: min(780px, 100%);
		background: rgba(14, 18, 19, 0.84);
		backdrop-filter: blur(22px) saturate(1.2);
		-webkit-backdrop-filter: blur(22px) saturate(1.2);
		border: 1px solid rgba(255, 255, 255, 0.07);
		border-radius: 26px;
		box-shadow: 0 50px 120px rgba(0, 0, 0, 0.5);
		overflow: hidden;
	}

	.bar {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		padding: 18px 20px 0 22px;
		border-bottom: 1px solid var(--rule);
	}
	.tabs {
		display: flex;
		gap: 2px;
	}
	.tabs button {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font: inherit;
		font-size: 16.5px;
		color: var(--ash);
		background: none;
		border: 0;
		padding: 9px 15px 11px;
		border-radius: 12px 12px 0 0;
		cursor: pointer;
	}
	.tabs button:hover {
		color: var(--stone);
	}
	.tabs button.on {
		color: var(--bone);
		background: rgba(255, 255, 255, 0.045);
	}
	.link {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-size: 13.5px;
		padding: 5px 12px 5px 10px;
		margin-bottom: 10px;
		border-radius: 99px;
		background: rgba(255, 255, 255, 0.07);
		white-space: nowrap;
	}
	.link i {
		width: 6px;
		height: 6px;
		border-radius: 50%;
		background: var(--mint);
	}

	.body {
		padding: 22px 26px 24px;
		min-height: 520px;
	}
	.cmd {
		margin: 0 0 18px;
	}

	.log,
	.after {
		list-style: none;
		margin: 0;
		padding: 0;
		font-family: var(--mono);
		font-size: 14px;
		line-height: 1.95;
	}
	.log li,
	.after,
	.chart {
		animation: rise 0.42s cubic-bezier(0.2, 0.7, 0.2, 1) both;
		animation-delay: calc(var(--i) * 110ms);
	}
	.log li {
		display: flex;
		gap: 12px;
	}
	.g {
		width: 1ch;
		flex: none;
		text-align: center;
	}
	.step {
		color: var(--bone);
	}
	.step .g {
		color: var(--stone);
	}
	.note {
		color: var(--stone);
		white-space: pre;
	}
	.log .note .t {
		padding-left: 2ch;
	}
	.ok .g,
	.ok {
		color: var(--mint);
	}
	.warn {
		color: var(--amber);
	}
	.warn .t {
		color: var(--bone);
	}

	@keyframes rise {
		from {
			opacity: 0;
			transform: translateY(5px);
		}
	}

	.chart {
		margin: 22px 0 18px;
		padding: 16px 20px 12px;
		border-radius: 16px;
		background: rgba(0, 0, 0, 0.32);
		border: 1px solid rgba(255, 255, 255, 0.05);
	}
	figcaption {
		display: flex;
		justify-content: space-between;
		gap: 12px;
		font-size: 13.5px;
		color: var(--stone);
		margin-bottom: 20px;
	}
	.range {
		color: var(--amber);
		font-size: 10.5px;
	}
	.range.calm {
		color: var(--iris);
	}
	.bars {
		position: relative;
		height: 96px;
		display: flex;
		align-items: flex-end;
		gap: 3px;
	}
	.bars span {
		flex: 1;
		height: var(--h);
		background: #4d5254;
		border-radius: 1.5px;
		transform-origin: bottom;
		animation: grow 0.5s cubic-bezier(0.2, 0.7, 0.2, 1) both;
		animation-delay: calc(var(--i) * 110ms + var(--j) * 14ms);
	}
	.bars span.spike {
		background: var(--amber);
	}
	.bars span.after {
		background: var(--mint);
	}
	.bars span.ssh {
		background: var(--iris);
	}
	@keyframes grow {
		from {
			transform: scaleY(0);
		}
	}
	.mark {
		position: absolute;
		top: -6px;
		transform: translateX(-30%);
		font-style: normal;
		font-size: 10px;
	}
	.axis {
		display: flex;
		justify-content: space-between;
		font-size: 10px;
		margin-top: 6px;
		color: var(--ash);
	}

	.after {
		font-size: 13.5px;
		line-height: 1.9;
	}
	.after li {
		display: flex;
		gap: 12px;
	}
	.after .note {
		color: var(--stone);
		white-space: normal;
	}

	.prompt {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 18px 26px 20px;
		border-top: 1px solid var(--rule);
		font-size: 17px;
	}
	.caret {
		color: var(--stone);
	}
	.cursor {
		width: 9px;
		height: 20px;
		background: var(--bone);
		margin-left: -6px;
		animation: blink 1.1s steps(1) infinite;
	}
	@keyframes blink {
		50% {
			opacity: 0;
		}
	}
	.keys {
		margin-left: auto;
		font-size: 10.5px;
	}

	.facts {
		display: grid;
		grid-template-columns: repeat(2, minmax(0, 1fr));
		gap: 32px 64px;
		margin-top: 80px;
	}
	.facts h3 {
		font-size: 20px;
		letter-spacing: -0.02em;
		margin: 0 0 6px;
	}
	.facts p {
		margin: 0;
		color: var(--stone);
		font-size: 15px;
	}
	.facts p {
		max-width: 52ch;
	}

	@media (max-width: 960px) {
		.facts {
			grid-template-columns: 1fr 1fr;
		}
	}
	@media (max-width: 640px) {
		.band {
			padding: 48px 10px;
		}
		.tabs button {
			font-size: 14.5px;
			padding: 8px 10px 10px;
		}
		.tabs button svg {
			display: none;
		}
		.bar {
			padding: 14px 12px 0;
		}
		.link {
			display: none;
		}
		.body {
			padding: 18px 16px;
			min-height: 0;
		}
		.log {
			font-size: 12.5px;
		}
		.note {
			white-space: pre-wrap;
		}
		.keys {
			display: none;
		}
		.prompt {
			font-size: 14.5px;
			padding: 14px 16px;
		}
		.facts {
			grid-template-columns: 1fr;
			gap: 28px;
		}
	}
</style>
