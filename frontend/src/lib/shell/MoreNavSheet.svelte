<!-- The phone's More sheet: the destinations from `navItems` the bottom bar has no room for. -->
<script lang="ts">
	import Modal from '$lib/ui/Modal.svelte';
	import NavIcon from './NavIcon.svelte';
	import NavItemBadge from './NavItemBadge.svelte';
	import { canSee, navHref, navItems, type NavViewer } from './navItems';

	interface Props {
		open: boolean;
		currentPath: string;
		viewer: NavViewer;
		updateAvailable: boolean;
	}

	let { open = $bindable(false), currentPath, viewer, updateAvailable }: Props = $props();

	const items = $derived(navItems.filter((item) => item.more && canSee(item, viewer)));
</script>

<Modal bind:open title="More" size="sm" bottomSheet closeButton>
	<ul class="dn-more menu -mx-4 -mt-2 w-auto gap-0.5">
		{#each items as item (item.id)}
			{@const active = item.more?.active(currentPath) ?? false}
			<li>
				<a
					href={navHref(item, updateAvailable)}
					class:menu-active={active}
					aria-current={active ? 'page' : undefined}
					aria-label={item.label}
					onclick={() => (open = false)}
				>
					<NavIcon {item}>
						{#if item.badge && item.more?.badge !== false}
							<NavItemBadge badge={item.badge} place="more" {updateAvailable} />
						{/if}
					</NavIcon>
					{item.label}
				</a>
			</li>
		{/each}
	</ul>
</Modal>

<style>
	/* the rail's look: 44px rows, the current one on the action fill */
	.dn-more :global(li > a) {
		min-height: 2.75rem;
		border-radius: 0.5rem;
		font-weight: 600;
	}
	.dn-more :global(li > a.menu-active) {
		background-color: var(--dn-action);
		color: var(--dn-action-content);
		box-shadow: inset 0 0 0 1px var(--dn-action-edge);
	}
</style>
