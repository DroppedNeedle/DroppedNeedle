<!-- Shows the error that `errorModal` holds. -->
<script lang="ts">
	import { Info, TriangleAlert } from 'lucide-svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { errorModal } from '$lib/stores/errorModal';
</script>

<Modal open={$errorModal.show} title={$errorModal.title} onclose={() => errorModal.hide()}>
	<div class="flex flex-col items-center text-center">
		<div class="bg-error/10 rounded-full p-3 mb-4">
			<TriangleAlert class="h-8 w-8 text-error" aria-hidden="true" />
		</div>
		<p class="text-sm text-base-content/70 leading-relaxed">
			{$errorModal.message}
		</p>
	</div>

	{#if $errorModal.details}
		<div class="mt-4 rounded-box bg-base-300/60 border border-base-300 p-4">
			<div class="flex gap-3 items-start">
				<Info class="h-5 w-5 text-info shrink-0 mt-0.5" aria-hidden="true" />
				<p class="text-sm text-base-content/80 leading-relaxed text-left">
					{$errorModal.details}
				</p>
			</div>
		</div>
	{/if}

	{#snippet actions()}
		<Button variant="accent" size="sm" onclick={() => errorModal.hide()}>Dismiss</Button>
	{/snippet}
</Modal>
