<script lang="ts">
	let { class: className = 'h-6 w-6', style }: { class?: string; style?: string } = $props();
</script>

<svg
	xmlns="http://www.w3.org/2000/svg"
	viewBox="0 0 24 24"
	fill="none"
	class={className}
	{style}
	aria-hidden="true"
>
	<polygon points="5.25,4 19.75,12 5.25,20" fill="currentColor" />
</svg>
