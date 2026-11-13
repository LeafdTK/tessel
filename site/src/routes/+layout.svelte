<script lang="ts">
	import '../app.css';
	import favicon from '#lib/assets/favicon.svg';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>Tessel: a multiplexer for coding agents</title>
	<meta
		name="description"
		content="A Mac app for running a bunch of coding agents at once. Each gets its own worktree, they keep running when you close the window, and anything you can click you can script."
	/>
	<meta name="theme-color" content="#07080a" />
</svelte:head>

{@render children()}
