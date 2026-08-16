<script lang="ts">
	import { resolve } from '$app/paths';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';

	onMount(() => void goto(resolve('/library/review'), { replaceState: true }));
</script>

<svelte:head><title>Identification review · DroppedNeedle</title></svelte:head>
