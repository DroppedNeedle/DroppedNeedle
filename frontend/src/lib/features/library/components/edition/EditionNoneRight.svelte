<!--
	"None of these is right" (admins), at the end of the edition list: keep
	the files as they are under the selected release's album, or leave the
	album unmanaged. Each says what it does before the click; both can be
	undone from the page.
-->
<script lang="ts">
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import { LEAVE_UNMANAGED_SENTENCE, customEditionSentence } from './editionCopy';

	interface Props {
		/** The selected release's album; without one there is nothing to keep the files under. */
		album: string | null;
		/** False when the album is already left unmanaged by the person. */
		canLeave: boolean;
		/** What the last action did, shown in place. */
		done: string | null;
		error: string | null;
		onkeep: () => Promise<void>;
		onleave: () => Promise<void>;
	}

	let { album, canLeave, done, error, onkeep, onleave }: Props = $props();
</script>

{#if album || canLeave || done || error}
	<section class="flex flex-col gap-3" aria-labelledby="edition-none-right">
		<h3
			id="edition-none-right"
			class="text-xs font-semibold tracking-wide text-base-content/55 uppercase"
		>
			None of these is right
		</h3>
		{#if album}
			<div class="flex flex-col items-start gap-1">
				<AsyncButton variant="outline" size="sm" onclick={onkeep}
					>Keep your own track list</AsyncButton
				>
				<p class="text-sm text-base-content/65">{customEditionSentence(album)}</p>
			</div>
		{/if}
		{#if canLeave}
			<div class="flex flex-col items-start gap-1">
				<AsyncButton variant="outline" size="sm" onclick={onleave}
					>Leave this album unmanaged</AsyncButton
				>
				<p class="text-sm text-base-content/65">{LEAVE_UNMANAGED_SENTENCE}</p>
			</div>
		{/if}
		{#if error}
			<p class="text-sm text-error" role="alert">{error}</p>
		{:else if done}
			<p class="text-sm text-success" role="status">{done}</p>
		{/if}
	</section>
{/if}
