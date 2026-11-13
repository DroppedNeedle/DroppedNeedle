<script lang="ts">
	import { goto } from '$app/navigation';
	import { withBasePath } from '$lib/utils/basePath';
	import { onMount } from 'svelte';

	onMount(() => void goto(withBasePath('/library/review'), { replaceState: true }));
</script>

<svelte:head><title>Identification review - DroppedNeedle</title></svelte:head>
