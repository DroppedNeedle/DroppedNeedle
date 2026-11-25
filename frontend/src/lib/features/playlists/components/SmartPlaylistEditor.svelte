<script lang="ts">
	import { untrack } from 'svelte';
	import { ApiError } from '$lib/api/errors';
	import {
		createSaveSmartPlaylistMutation,
		type SmartPlaylist
	} from '../queries/SmartPlaylistQueries.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	let { playlist, onclose }: { playlist?: SmartPlaylist; onclose: () => void } = $props();
	let name = $state(untrack(() => playlist?.name ?? ''));
	let stars = $state(untrack(() => String(playlist?.min_rating ?? 5)));
	const ratings = [1, 2, 3, 4, 5].map((n) => ({
		value: String(n),
		label: n === 1 ? '1 star' : n + ' stars'
	}));
	const mutation = createSaveSmartPlaylistMutation();

	function save(event: SubmitEvent) {
		event.preventDefault();
		const trimmed = name.trim();
		const userId = authStore.user?.id;
		if (!userId || !trimmed || mutation.isPending) return;
		mutation.mutate(
			{ id: playlist?.id, input: { name: trimmed, min_rating: Number(stars) }, userId },
			{ onSuccess: onclose }
		);
	}
</script>

<Modal
	open
	title={playlist ? 'Edit smart playlist' : 'New smart playlist'}
	dismissible={!mutation.isPending}
	{onclose}
>
	<form class="space-y-4" onsubmit={save}>
		<TextField label="Playlist name" bind:value={name} disabled={mutation.isPending} required />
		<SelectField
			label="My rating is at least"
			options={ratings}
			bind:value={stars}
			disabled={mutation.isPending}
		/>
		{#if mutation.isError}
			<p class="text-error" role="alert">
				{mutation.error instanceof ApiError && mutation.error.status === 400
					? mutation.error.message
					: "Couldn't save the smart playlist. Try again."}
			</p>
		{/if}
		<div class="flex justify-end gap-2">
			<Button variant="ghost" disabled={mutation.isPending} onclick={onclose}>Cancel</Button>
			<Button type="submit" variant="primary" loading={mutation.isPending} disabled={!name.trim()}
				>Save</Button
			>
		</div>
	</form>
</Modal>
