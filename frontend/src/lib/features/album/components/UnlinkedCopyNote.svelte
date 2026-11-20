<!--
	Points at library albums with no MusicBrainz match that share the page's
	title and album artist, so nobody fetches a copy they already hold. A
	name is never proof: the note only links to the copy, and a curator can
	open its edition chooser from here.
-->
<script lang="ts">
	import type { components } from '$lib/api/v3/openapi';
	import { HardDrive } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import { albumHref } from '$lib/utils/entityRoutes';
	import { authStore } from '$lib/stores/authStore.svelte';

	type UnlinkedCopy = components['schemas']['UnlinkedCopy'];

	interface Props {
		copies: UnlinkedCopy[];
	}

	let { copies }: Props = $props();

	function details(copy: UnlinkedCopy): string {
		const parts = [
			copy.title,
			`${copy.track_count} ${copy.track_count === 1 ? 'track' : 'tracks'}`
		];
		if (copy.format) parts.push(copy.format.toUpperCase());
		parts.push('not linked to MusicBrainz yet');
		return parts.join(' · ');
	}
</script>

<Card
	variant="note"
	padding="sm"
	as="section"
	class="w-full"
	aria-label="You may already have this"
>
	<div class="flex items-start gap-3">
		<HardDrive class="mt-0.5 size-5 shrink-0 text-action-edge" aria-hidden="true" />
		<div class="flex min-w-0 grow flex-col gap-2">
			<p class="text-sm font-semibold">You may already have this</p>
			{#each copies as copy (copy.id)}
				<div class="flex flex-wrap items-center gap-x-3 gap-y-2">
					<p class="min-w-0 grow text-xs text-muted">{details(copy)}</p>
					<div class="flex flex-wrap items-center gap-2">
						<Button variant="outline" size="sm" href={albumHref(copy.id)}>Open your copy</Button>
						{#if authStore.isTrusted}
							<Button variant="ghost" size="sm" href={`${albumHref(copy.id)}?edition=choose`}>
								Pick its edition
							</Button>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	</div>
</Card>
