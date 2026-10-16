<!-- The phone's More sheet: the destinations from `navItems` the bottom bar has no room for. -->
<script lang="ts">
	import { X } from 'lucide-svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
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

	let dialog = $state<HTMLDialogElement>();

	const items = $derived(navItems.filter((item) => item.more && canSee(item, viewer)));

	$effect(() => {
		if (!dialog) return;
		if (open && !dialog.open) dialog.showModal();
		else if (!open && dialog.open) dialog.close();
	});
</script>

<dialog
	bind:this={dialog}
	class="modal modal-bottom sm:modal-middle"
	aria-label="More navigation"
	onclose={() => (open = false)}
>
	<div class="modal-box p-2">
		<form method="dialog">
			<IconButton type="submit" size="sm" icon={X} label="Close" class="absolute right-2 top-2" />
		</form>
		<h3 class="font-bold text-lg px-2 pt-1 pb-2">More</h3>
		<ul class="menu w-full">
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
	</div>
	<form method="dialog" class="modal-backdrop">
		<button aria-label="Close more navigation">close</button>
	</form>
</dialog>
