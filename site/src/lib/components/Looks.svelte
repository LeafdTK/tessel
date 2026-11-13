<section id="looks" class="wrap">
	<div class="head">
		<h2>Themes</h2>
		<p class="lede">
			Twelve themes, light and dark, with solid, glass or felt panes. The layout and colors live in
			<code>~/.tessel/ui.json</code>, which reloads when you save it. The dithered backgrounds on this
			page come from one of the backdrop options.
		</p>
	</div>

	<div class="pair">
		<img
			class="back"
			src="/shots/themes.webp"
			alt="Settings, Themes: material (solid, glass, felt), backdrop (none, dither, space) and density."
			width="1600"
			height="1059"
			loading="lazy"
		/>
		<img
			class="front"
			src="/shots/light.webp"
			alt="Tessel in a light theme."
			width="1600"
			height="1059"
			loading="lazy"
		/>
	</div>
</section>

<style>
	section {
		padding: 140px 0 120px;
	}
	.head {
		margin-bottom: 56px;
	}
	h2 {
		font-size: clamp(34px, 4vw, 52px);
		margin-bottom: 20px;
	}
	.lede {
		margin: 0;
		color: var(--stone);
		font-size: 17px;
		max-width: 58ch;
	}
	code {
		color: var(--bone);
		background: rgba(236, 230, 219, 0.07);
		padding: 1px 6px;
		border-radius: 6px;
		font-size: 0.9em;
	}
	.pair {
		position: relative;
		padding-bottom: 22%;
	}
	.pair img {
		border-radius: 14px;
		box-shadow:
			0 40px 90px rgba(0, 0, 0, 0.55),
			0 0 0 1px var(--rule);
	}
	.back {
		width: 74%;
	}
	.front {
		position: absolute;
		width: 56%;
		right: 0;
		bottom: 0;
	}
	@media (max-width: 760px) {
		.back {
			width: 92%;
		}
		.front {
			width: 70%;
		}
	}
</style>
