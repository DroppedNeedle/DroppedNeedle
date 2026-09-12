<script lang="ts">
	interface Props {
		trackTitle: string;
		removing: boolean;
		error: string | null;
		onconfirm: () => void;
		onclose: () => void;
	}

	let { trackTitle, removing, error, onconfirm, onclose }: Props = $props();

	let dialogEl: HTMLDialogElement | undefined = $state();

	$effect(() => {
		if (dialogEl && !dialogEl.open) {
			dialogEl.showModal();
		}
	});

	function handleCancel(event: Event) {
		if (removing) {
			event.preventDefault();
			return;
		}
		dialogEl?.close();
	}
</script>

<dialog
	bind:this={dialogEl}
	class="modal"
	aria-labelledby="remove-file-dialog-title"
	{onclose}
	oncancel={handleCancel}
>
	<div class="modal-box max-w-md">
		<h3 id="remove-file-dialog-title" class="text-lg font-bold">Remove File</h3>
		<p class="py-4 text-base-content/70">
			Remove <span class="font-semibold text-base-content">{trackTitle}</span> from your library? This
			deletes the file from disk - this can't be undone.
		</p>
		{#if error}
			<p class="pb-3 text-sm text-error" role="alert">{error}</p>
		{/if}
		<div class="modal-action">
			<!-- svelte-ignore a11y_autofocus -->
			<button class="btn btn-sm" onclick={() => dialogEl?.close()} disabled={removing} autofocus>
				Cancel
			</button>
			<button class="btn btn-error btn-sm" onclick={onconfirm} disabled={removing}>
				{#if removing}<span class="loading loading-spinner loading-xs"></span>{/if}
				Remove
			</button>
		</div>
	</div>
</dialog>
