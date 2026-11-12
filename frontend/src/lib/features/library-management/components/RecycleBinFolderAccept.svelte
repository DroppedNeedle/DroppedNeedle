<!--
	"Use this folder", for a recycle bin path whose folder is not the one the
	bin was in before: after a confirm, the server marks the folder as the bin
	and removals and upgrades go ahead.
-->
<script lang="ts">
	import { createAcceptRecycleBinFolderMutation } from '$lib/features/library-management/queries/RecycleBinQueries.svelte';
	import Button from '$lib/ui/Button.svelte';
	import ConfirmDialog from '$lib/ui/ConfirmDialog.svelte';

	interface Props {
		/** The saved recycle bin path the warning is about. */
		path: string;
	}

	let { path }: Props = $props();
	const accept = createAcceptRecycleBinFolderMutation();
	let open = $state(false);
</script>

<Button variant="paper" size="sm" onclick={() => (open = true)}>Use this folder</Button>

<ConfirmDialog
	bind:open
	title="Use this folder as the recycle bin?"
	confirmLabel="Use this folder"
	tone="warning"
	paper
	pending={accept.isPending}
	onconfirm={() => accept.mutateAsync(path)}
>
	<p>
		Only if you moved the bin. If its share just isn't mounted, mount it instead, and restart
		DroppedNeedle if it started while the share was down.
	</p>
	<p class="mt-3 font-mono text-sm break-all">{path}</p>
</ConfirmDialog>
