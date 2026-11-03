<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';

	interface Props {
		playlistName: string;
		deleting: boolean;
		onconfirm: () => void;
	}

	let { playlistName, deleting, onconfirm }: Props = $props();

	let open = $state(false);

	export function showModal() {
		open = true;
	}
</script>

<Modal bind:open title={`Delete "${playlistName}"?`} dismissible={!deleting}>
	<p class="text-base-content/70">
		This will permanently remove the playlist and all its tracks. This action cannot be undone.
	</p>
	{#snippet actions()}
		<Button variant="ghost" onclick={() => (open = false)} disabled={deleting}>Cancel</Button>
		<Button variant="danger" loading={deleting} onclick={onconfirm}>Delete</Button>
	{/snippet}
</Modal>
