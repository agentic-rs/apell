const menu = document.querySelector('.site-menu');

menu?.addEventListener('click', (event) => {
  if (event.target instanceof Element && event.target.closest('nav a[href^="#"]')) {
    menu.open = false;
  }
});

menu?.addEventListener('keydown', (event) => {
  if (event.key === 'Escape' && menu.open) {
    menu.open = false;
    menu.querySelector('summary').focus();
  }
});
