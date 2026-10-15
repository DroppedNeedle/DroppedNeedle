<script lang="ts">
	import Modal from './Modal.svelte';

	interface Props {
		dismissible?: boolean;
		size?: 'md' | 'sheet';
		description?: string;
		onclose?: () => void;
	}

	let { dismissible = true, size = 'md', description, onclose }: Props = $props();

	let open = $state(false);
	let mounted = $state(true);
</script>

<button type="button" onclick={() => (open = true)}>Open settings</button>
<p>{open ? 'Dialog is open' : 'Dialog is closed'}</p>

{#if mounted}
	<Modal bind:open title="Rename playlist" {size} {description} {dismissible} {onclose}>
		<input aria-label="Playlist name" />
		{#snippet actions()}
			<button type="button" onclick={() => (open = false)}>Cancel</button>
			<button type="button" onclick={() => (mounted = false)}>Remove dialog</button>
		{/snippet}
	</Modal>
{/if}
