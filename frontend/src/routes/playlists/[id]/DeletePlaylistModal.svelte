<script lang="ts">
	interface Props {
		playlistName: string;
		deleting: boolean;
		onconfirm: () => void;
	}

	let { playlistName, deleting, onconfirm }: Props = $props();

	let dialogEl = $state<HTMLDialogElement | null>(null);

	export function showModal() {
		dialogEl?.showModal();
	}
</script>

<dialog bind:this={dialogEl} class="modal">
	<div class="modal-box rounded-2xl border border-base-content/10 bg-base-200">
		<h3 class="font-display text-lg font-bold tracking-tight">Delete "{playlistName}"?</h3>
		<p class="py-4 text-sm text-base-content/70">
			This will permanently remove the playlist and all its tracks. This action cannot be undone.
		</p>
		<div class="modal-action">
			<form method="dialog">
				<button class="btn btn-ghost rounded-full bg-base-content/6">Cancel</button>
			</form>
			<button class="btn btn-error rounded-full" onclick={onconfirm} disabled={deleting}>
				{#if deleting}
					<span class="loading loading-spinner loading-xs"></span>
				{/if}
				Delete
			</button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>
