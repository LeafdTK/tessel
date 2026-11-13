<script lang="ts">
	let { size = 22 }: { size?: number } = $props();
</script>

<!-- the two arcs from tessel's truchet icon -->
<svg width={size} height={size} viewBox="0 0 24 24" aria-hidden="true">
	<path
		d="M12.5 4.5 A7.5 7.5 0 0 1 5 12 M19.5 12 A7.5 7.5 0 0 0 12 19.5"
		fill="none"
		stroke="currentColor"
		stroke-width="3.2"
		stroke-linecap="round"
	/>
</svg>
