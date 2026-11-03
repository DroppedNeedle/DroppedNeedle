<script lang="ts">
	let { class: className = 'h-6 w-6' }: { class?: string } = $props();
</script>

<svg
	xmlns="http://www.w3.org/2000/svg"
	viewBox="0 0 24 24"
	fill="none"
	stroke="currentColor"
	stroke-width="2"
	stroke-linecap="round"
	stroke-linejoin="round"
	class={className}
	aria-hidden="true"
>
	<circle cx="12" cy="12" r="10" />
	<polygon points="10,8 16,12 10,16" fill="currentColor" stroke="none" />
</svg>
