<script lang="ts">
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';

	interface Props {
		trackTitle: string;
		removing: boolean;
		error: string | null;
		onconfirm: () => void;
		onclose: () => void;
	}

	let { trackTitle, removing, error, onconfirm, onclose }: Props = $props();

	let open = $state(true);

	function cancel() {
		open = false;
		onclose();
	}
</script>

<Modal bind:open title="Remove File" dismissible={!removing} {onclose}>
	<p class="text-base-content/70">
		Remove <span class="font-semibold text-base-content">{trackTitle}</span> from your library? This deletes
		the file from disk - this can't be undone.
	</p>
	{#if error}
		<p class="pt-3 text-sm text-error" role="alert">{error}</p>
	{/if}
	{#snippet actions()}
		<Button size="sm" onclick={cancel} disabled={removing} autofocus>Cancel</Button>
		<Button variant="danger" size="sm" loading={removing} onclick={onconfirm}>Remove</Button>
	{/snippet}
</Modal>
