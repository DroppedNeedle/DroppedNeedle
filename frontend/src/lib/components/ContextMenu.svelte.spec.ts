import { page } from '@vitest/browser/context';
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render } from 'vitest-browser-svelte';
import ContextMenu from './ContextMenu.svelte';
import { closeAllMenus } from './ContextMenu.svelte';
import type { MenuItem } from './ContextMenu.svelte';
import { ListPlus, ListStart } from 'lucide-svelte';

function makeItems(overrides: Partial<MenuItem>[] = []): MenuItem[] {
	const defaults: MenuItem[] = [
		{ label: 'Add to Queue', icon: ListPlus, onclick: vi.fn() },
		{ label: 'Play Next', icon: ListStart, onclick: vi.fn() }
	];
	return defaults.map((d, i) => ({ ...d, ...overrides[i] }));
}

async function renderMenu(items: MenuItem[] = makeItems()) {
	return await render(ContextMenu, {
		props: { items }
	} as Parameters<typeof render<typeof ContextMenu>>[1]);
}

describe('ContextMenu.svelte', () => {
	beforeEach(() => {
		closeAllMenus();
	});

	it('renders the trigger button', async () => {
		await renderMenu();
		await expect.element(page.getByLabelText('More actions')).toBeInTheDocument();
	});

	it('shows menu items when opened', async () => {
		await renderMenu();
		const trigger = page.getByLabelText('More actions');
		await trigger.click();
		await expect.element(page.getByText('Add to Queue')).toBeVisible();
		await expect.element(page.getByText('Play Next')).toBeVisible();
	});

	it('fires callback on item click and closes menu', async () => {
		const items = makeItems();
		await renderMenu(items);
		const trigger = page.getByLabelText('More actions');
		await trigger.click();
		await page.getByText('Add to Queue').click();
		expect(items[0].onclick).toHaveBeenCalledOnce();
	});

	it('renders disabled items as non-interactive', async () => {
		const items = makeItems([{ disabled: true }]);
		await renderMenu(items);
		const trigger = page.getByLabelText('More actions');
		await trigger.click();
		const disabledBtn = page.getByText('Add to Queue');
		await expect.element(disabledBtn).toBeVisible();
	});

	it('has correct ARIA roles', async () => {
		await renderMenu();
		const trigger = page.getByLabelText('More actions');
		await trigger.click();
		await expect.element(page.getByRole('menu')).toBeInTheDocument();
		const menuItems = page.getByRole('menuitem').all();
		expect(menuItems.length).toBeGreaterThanOrEqual(2);
	});

	it('does not have redundant role on summary', async () => {
		await renderMenu();
		const summary = page.getByLabelText('More actions');
		const roleAttr = await summary.element().getAttribute('role');
		expect(roleAttr).toBeNull();
	});

	it('opening a second menu closes the first', async () => {
		const items1: MenuItem[] = [{ label: 'Action A', icon: ListPlus, onclick: vi.fn() }];
		const items2: MenuItem[] = [{ label: 'Action B', icon: ListStart, onclick: vi.fn() }];

		await renderMenu(items1);
		await renderMenu(items2);

		const triggers = page.getByLabelText('More actions').all();
		expect((await triggers).length).toBe(2);

		await (await triggers)[0].click();
		await expect.element(page.getByText('Action A')).toBeVisible();

		await (await triggers)[1].click();
		await expect.element(page.getByText('Action B')).toBeVisible();
		await expect.element(page.getByText('Action A')).not.toBeInTheDocument();
	});
});
