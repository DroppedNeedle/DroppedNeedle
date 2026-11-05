<!--
	A playlist page that has nothing to show: it failed to load, it is
	missing, or it is someone else's private playlist. The icon, the Anton
	title, one line, Retry when trying again can help, and a round way back
	to Playlists. The title is the page's h1.
-->
<script lang="ts">
	import { ArrowLeft, RefreshCw } from 'lucide-svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import Button, { type KitIcon } from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';

	interface Props {
		icon: KitIcon;
		title: string;
		message?: string;
		onretry?: () => void;
	}

	let { icon: Icon, title, message, onretry }: Props = $props();
</script>

<div class="pl-notice">
	<Icon class="pl-notice__icon" aria-hidden="true" strokeWidth={1.5} />
	<h1 class="pl-notice__title">{title}</h1>
	{#if message}<p class="pl-notice__message">{message}</p>{/if}
	<div class="pl-notice__actions">
		{#if onretry}
			<Button variant="primary" size="display-sm" icon={RefreshCw} onclick={onretry}>Retry</Button>
		{/if}
		<IconButton
			variant="outline"
			icon={ArrowLeft}
			label="Back to Playlists"
			href={withBasePath('/playlists')}
		/>
	</div>
</div>

<style>
	.pl-notice {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.75rem;
		padding: 5rem 1rem;
		text-align: center;
	}
	.pl-notice :global(.pl-notice__icon) {
		width: 4.5rem;
		height: 4.5rem;
		margin-bottom: 0.5rem;
		color: var(--dn-muted);
	}
	.pl-notice__title {
		font-size: clamp(2rem, 1.4rem + 2.4vw, 3rem);
		line-height: 1;
		text-wrap: balance;
	}
	.pl-notice__message {
		max-width: 32rem;
		font-size: 1rem;
		color: var(--dn-muted);
	}
	.pl-notice__actions {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		margin-top: 0.75rem;
	}
</style>
