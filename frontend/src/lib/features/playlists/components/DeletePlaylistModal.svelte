<script lang="ts">
	import { Trash2 } from 'lucide-svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	interface Props {
		playlistName: string;
		deleting: boolean;
		onconfirm: () => unknown;
	}

	let { playlistName, deleting, onconfirm }: Props = $props();

	let open = $state(false);

	export function showModal() {
		open = true;
	}
</script>

<ConfirmDialog
	bind:open
	title="Delete playlist"
	icon={Trash2}
	tone="danger"
	confirmLabel="Delete"
	pending={deleting}
	{onconfirm}
>
	<p class="font-semibold">Delete "{playlistName}"?</p>
	<p class="mt-2 text-muted">
		This will permanently remove the playlist. It won't delete any tracks from your library.
	</p>
</ConfirmDialog>
