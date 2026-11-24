<!-- A year the person has no review for, in the app's not-found words. -->
<script lang="ts">
	import { SearchX } from 'lucide-svelte';
	import EmptyState from '$lib/ui/EmptyState.svelte';
</script>

<svelte:head>
	<title>Year not found - DroppedNeedle</title>
</svelte:head>

<EmptyState
	icon={SearchX}
	title="Off the record"
	description="That year isn't in the crate."
	ctaLabel="Your year"
	ctaHref="/your-year"
/>
