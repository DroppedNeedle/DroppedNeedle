<script lang="ts">
	import PageHeader from '$lib/components/PageHeader.svelte';
	import UnconfirmedAlbums from '$lib/components/library/UnconfirmedAlbums.svelte';
</script>

<svelte:head><title>Unconfirmed matches · DroppedNeedle</title></svelte:head>

<div class="min-h-[calc(100vh-200px)]">
	<PageHeader
		subtitle="Albums whose edition is a best guess, or that match nothing yet"
		backHref="/library"
		backLabel="Back to library"
	>
		{#snippet title()}Unconfirmed matches{/snippet}
	</PageHeader>
	<div class="mx-auto max-w-[96rem] px-4 pb-12 sm:px-6 lg:px-8">
		<UnconfirmedAlbums />
	</div>
</div>
