<script lang="ts">
	import { ImagePlus } from 'lucide-svelte';
	import { getApiUrl } from '$lib/api/api-utils';
	import { createUploadAvatarMutation } from '$lib/queries/profile/ProfileMutations.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';

	interface Props {
		open: boolean;
	}

	let { open = $bindable(false) }: Props = $props();

	const avatarMutation = createUploadAvatarMutation(authStore.user?.id ?? '');

	let avatarPreview: string | null = $state(null);
	let avatarFile: File | null = $state(null);
	let draggingOver = $state(false);
	let fileInput: HTMLInputElement | undefined = $state();

	function handleAvatarFile(file: File) {
		if (!file.type.startsWith('image/')) return;
		avatarFile = file;
		if (avatarPreview) URL.revokeObjectURL(avatarPreview);
		avatarPreview = URL.createObjectURL(file);
	}

	function handleDrop(e: DragEvent) {
		e.preventDefault();
		draggingOver = false;
		const file = e.dataTransfer?.files[0];
		if (file) handleAvatarFile(file);
	}

	function handleDragOver(e: DragEvent) {
		e.preventDefault();
		draggingOver = true;
	}

	function handleFileSelect(e: Event) {
		const input = e.target as HTMLInputElement;
		const file = input.files?.[0];
		if (file) handleAvatarFile(file);
	}

	async function uploadAvatar() {
		if (!avatarFile) return;
		try {
			await avatarMutation.mutateAsync(avatarFile);
			close();
		} catch {
			// keep the dialog open to retry; failure shows via disabled/loading state
		}
	}

	function close() {
		open = false;
		avatarFile = null;
		if (avatarPreview) {
			URL.revokeObjectURL(avatarPreview);
			avatarPreview = null;
		}
	}
</script>

<Modal bind:open title="Upload Profile Picture" size="sm" onclose={close}>
	<input
		type="file"
		accept="image/jpeg,image/png,image/webp,image/gif"
		class="hidden"
		aria-label="Choose a picture"
		bind:this={fileInput}
		onchange={handleFileSelect}
	/>
	<button
		type="button"
		class="flex w-full cursor-pointer flex-col items-center justify-center gap-3 rounded-box border-2 border-dashed p-6 transition-colors
			{draggingOver ? 'border-primary bg-primary/10' : 'border-base-content/20 hover:border-primary/50'}"
		ondrop={handleDrop}
		ondragover={handleDragOver}
		ondragleave={() => (draggingOver = false)}
		onclick={() => fileInput?.click()}
	>
		{#if avatarPreview}
			<img
				src={getApiUrl(avatarPreview)}
				alt="Preview"
				class="h-24 w-24 rounded-full object-cover ring-2 ring-base-content/10"
			/>
			<span class="text-xs text-base-content/60">{avatarFile?.name}</span>
		{:else}
			<ImagePlus class="h-10 w-10 text-base-content/30" />
			<span class="text-sm text-base-content/60">Drag & drop an image here, or click to browse</span
			>
			<span class="text-xs text-base-content/40">JPEG, PNG, WebP, or GIF, max 5 MB</span>
		{/if}
	</button>

	{#snippet actions()}
		<Button variant="ghost" size="sm" onclick={close}>Cancel</Button>
		<Button
			variant="primary"
			size="sm"
			loading={avatarMutation.isPending}
			disabled={!avatarFile}
			onclick={() => void uploadAvatar()}
		>
			Upload
		</Button>
	{/snippet}
</Modal>
