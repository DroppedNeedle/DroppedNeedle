<!--
	How a list on rotation is made, on its own page: the rule as one mono
	line, "Keep on rotation" (not on the two always-on lists), and "Save a
	copy", which opens the new playlist. Its owner is the only one who sees
	it.
-->
<script lang="ts">
	import { goto } from '$app/navigation';
	import type { RotationInfo } from './rotationIdeas';
	import {
		createCopyRotationMutation,
		createPinRotationMutation
	} from '$lib/features/playlists/queries/rotation.svelte';
	import { toastStore } from '$lib/stores/toast';
	import { withBasePath } from '$lib/utils/basePath';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Card from '$lib/ui/Card.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { ideaRule } from './rotationIdeas';

	interface Props {
		playlistId: string;
		rotation: RotationInfo;
	}

	let { playlistId, rotation }: Props = $props();

	const pin = createPinRotationMutation();
	const copy = createCopyRotationMutation();

	let pinned = $derived(rotation.pinned);

	async function setPinned(on: boolean) {
		pinned = on;
		try {
			await pin.mutateAsync({ playlistId, pinned: on });
		} catch {
			pinned = rotation.pinned;
			toastStore.show({ message: "Couldn't change that list", type: 'error' });
		}
	}

	async function saveCopy() {
		try {
			const made = await copy.mutateAsync(playlistId);
			toastStore.show({ message: 'Saved as your own playlist', type: 'success' });
			await goto(withBasePath(`/playlists/${made.id}`));
		} catch {
			toastStore.show({ message: "Couldn't save a copy", type: 'error' });
		}
	}
</script>

<Card variant="bordered" padding="sm">
	<section class="rot-band" aria-label="How this list is made">
		<p class="rot-band__rule">{ideaRule(rotation)}</p>
		{#if !rotation.always_on}
			<ToggleRow
				label="Keep on rotation"
				description="Stays one of your five; songs still change."
				disabled={pin.isPending}
				bind:checked={() => pinned, (on) => void setPinned(on)}
			/>
		{/if}
		<div class="rot-band__copy">
			<span class="text-sm text-muted">Keep these songs as your own playlist.</span>
			<AsyncButton variant="outline" size="display-sm" onclick={saveCopy}>Save a copy</AsyncButton>
		</div>
	</section>
</Card>

<style>
	.rot-band {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}
	.rot-band__rule {
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 600;
		letter-spacing: 0.1em;
		line-height: 1.6;
		text-transform: uppercase;
	}
	.rot-band__copy {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.75rem;
	}
</style>
