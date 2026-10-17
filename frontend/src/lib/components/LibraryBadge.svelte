<script lang="ts">
	import { Check, Clock, Trash2 } from 'lucide-svelte';
	import DeleteAlbumModal from './DeleteAlbumModal.svelte';
	import { authStore } from '$lib/stores/authStore.svelte';

	interface Props {
		status: 'library' | 'requested';
		musicbrainzId: string;
		albumTitle: string;
		artistName: string;
		size?: 'sm' | 'md' | 'lg';
		positioning?: string;
		ondeleted?: () => void;
	}

	let {
		status,
		musicbrainzId,
		albumTitle,
		artistName,
		size = 'md',
		positioning = '',
		ondeleted
	}: Props = $props();

	let showDeleteModal = $state(false);

	const sizeClasses = $derived(
		{
			sm: { button: 'p-1', icon: 'w-3.5 h-3.5', strokeWidth: status === 'library' ? '3' : '2' },
			md: { button: 'p-1.5', icon: 'h-5 w-5', strokeWidth: status === 'library' ? '3' : '2' },
			lg: {
				button: 'p-0',
				icon: 'h-5 w-5 sm:h-6 sm:w-6',
				strokeWidth: status === 'library' ? '3' : '2'
			}
		}[size]
	);

	const lgButtonClass = $derived(
		size === 'lg' ? 'w-8 h-8 sm:w-10 sm:h-10 flex items-center justify-center' : ''
	);
	const tone = $derived(
		status === 'library' ? 'bg-accent text-accent-content' : 'bg-warning text-warning-content'
	);

	function handleClick(e: Event) {
		e.stopPropagation();
		e.preventDefault();
		showDeleteModal = true;
	}

	function handleDeleted() {
		showDeleteModal = false;
		ondeleted?.();
	}
</script>

{#if authStore.isAdmin}
	<button
		class="{positioning} {tone} group/badge rounded-full shadow-sm transition-colors duration-200 hover:bg-error hover:text-error-content {sizeClasses.button} {lgButtonClass}"
		onclick={handleClick}
		aria-label={status === 'library' ? 'Remove from library' : 'Remove request'}
	>
		{#if status === 'library'}
			<Check
				class="{sizeClasses.icon} group-hover/badge:hidden"
				strokeWidth={Number(sizeClasses.strokeWidth)}
			/>
		{:else}
			<Clock
				class="{sizeClasses.icon} group-hover/badge:hidden"
				strokeWidth={Number(sizeClasses.strokeWidth)}
			/>
		{/if}
		<Trash2 class="{sizeClasses.icon} hidden group-hover/badge:block" strokeWidth={2} />
	</button>
{:else}
	<span
		class="{positioning} {tone} rounded-full shadow-sm {sizeClasses.button} {lgButtonClass}"
		aria-label={status === 'library' ? 'In library' : 'Requested'}
	>
		{#if status === 'library'}
			<Check class={sizeClasses.icon} strokeWidth={Number(sizeClasses.strokeWidth)} />
		{:else}
			<Clock class={sizeClasses.icon} strokeWidth={Number(sizeClasses.strokeWidth)} />
		{/if}
	</span>
{/if}

{#if showDeleteModal}
	<DeleteAlbumModal
		{albumTitle}
		{artistName}
		{musicbrainzId}
		ondeleted={handleDeleted}
		onclose={() => {
			showDeleteModal = false;
		}}
	/>
{/if}
