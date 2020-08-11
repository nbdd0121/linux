// SPDX-License-Identifier: GPL-2.0-only
#include <linux/clk.h>
#include <linux/err.h>
#include <linux/io.h>
#include <linux/mmc/host.h>
#include <linux/module.h>
#include <linux/of.h>

#include "sdhci-pltfm.h"

static const struct sdhci_ops sdhci_garyguo_ops = {
	.set_clock		= sdhci_set_clock,
	.set_bus_width		= sdhci_set_bus_width,
	.reset			= sdhci_reset,
	.set_uhs_signaling	= sdhci_set_uhs_signaling,
};

static const struct sdhci_pltfm_data sdhci_garyguo_pdata = {
	.ops = &sdhci_garyguo_ops,
};

static int sdhci_garyguo_probe(struct platform_device *pdev)
{
	struct sdhci_host *host;
	struct sdhci_pltfm_host *pltfm_host;
	int ret;

	host = sdhci_pltfm_init(pdev, &sdhci_garyguo_pdata, 0);
	if (IS_ERR(host))
		return PTR_ERR(host);

	pltfm_host = sdhci_priv(host);
	pltfm_host->clk = devm_clk_get(&pdev->dev, NULL);

	if (!IS_ERR(pltfm_host->clk))
		clk_prepare_enable(pltfm_host->clk);

	ret = mmc_of_parse(host->mmc);
	if (ret)
		goto err_sdhci_add;

	ret = sdhci_add_host(host);
	if (ret)
		goto err_sdhci_add;

	return 0;

err_sdhci_add:
	clk_disable_unprepare(pltfm_host->clk);
	sdhci_pltfm_free(pdev);
	return ret;
}

static const struct of_device_id sdhci_garyguo_of_match_table[] = {
	{ .compatible = "garyguo,sdhci", },
	{}
};
MODULE_DEVICE_TABLE(of, sdhci_garyguo_of_match_table);

static struct platform_driver sdhci_garyguo_driver = {
	.driver		= {
		.name	= "sdhci-garyguo",
		.pm	= &sdhci_pltfm_pmops,
		.of_match_table = sdhci_garyguo_of_match_table,
	},
	.probe		= sdhci_garyguo_probe,
	.remove		= sdhci_pltfm_unregister,
};

module_platform_driver(sdhci_garyguo_driver);

MODULE_DESCRIPTION("SDHCI platform driver for Gary Guo's SDHCI controller");
MODULE_AUTHOR("Gary Guo <gary@garyguo.net>");
MODULE_LICENSE("GPL v2");
