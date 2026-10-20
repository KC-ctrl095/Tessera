set -e
git checkout -q -b fix/readme-typos
sed -i 's/recieve/receive/g' README.md
git commit -q -am "Fix 'receive' typos in README"
git checkout -q main
